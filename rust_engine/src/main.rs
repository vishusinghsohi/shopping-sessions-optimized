mod attribution;
mod clicks;
mod csv_parser;
mod daily;
mod events;
mod identity;
mod sessions;
mod timestamp;

use identity::IdentityMap;

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

// =====================================================
// DISCOVER CSV PARTITIONS
// =====================================================

fn csv_files(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = fs::read_dir(directory)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<io::Result<Vec<_>>>()?;

    files.retain(|path| path.extension().is_some_and(|ext| ext == "csv"));

    files.sort();

    Ok(files)
}

// =====================================================
// LINKED USER IDENTITY RESOLUTION
// =====================================================

fn load_links(data_dir: &Path) -> io::Result<IdentityMap> {
    let mut identities = IdentityMap::new();

    let mut accepted = 0usize;
    let mut dropped = 0usize;

    for path in csv_files(&data_dir.join("links"))? {
        let mut reader =
            BufReader::with_capacity(csv_parser::PARTITION_BUFFER_BYTES, File::open(path)?);
        let mut line = Vec::new();

        // Skip the first physical header per CSV.
        reader.read_until(b'\n', &mut line)?;

        loop {
            line.clear();

            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }

            // Remove LF only, preserve CR.
            if line.last() == Some(&b'\n') {
                line.pop();
            }

            let Some(fields) = csv_parser::parse_line::<2>(&line) else {
                dropped += 1;
                continue;
            };

            if fields[0].is_empty() || fields[1].is_empty() {
                dropped += 1;
                continue;
            }

            let user_a = std::str::from_utf8(fields[0]).map_err(io::Error::other)?;

            let user_b = std::str::from_utf8(fields[1]).map_err(io::Error::other)?;

            identities.union(user_a, user_b);
            accepted += 1;
        }
    }

    println!("\nIDENTITY RESOLUTION");
    println!("  Accepted links: {accepted}");
    println!("  Dropped links: {dropped}");

    Ok(identities)
}

// =====================================================
// MAIN PROCESSING PIPELINE
// =====================================================

fn main() -> io::Result<()> {
    let input = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "fixtures/E1/input".to_owned());

    let data_dir = Path::new(&input);

    println!("========================================");
    println!("       SHOPPING SESSIONS ENGINE");
    println!("========================================");
    println!("Input: {}", data_dir.display());

    // PHASE 1: Resolve linked identities.
    let mut identities = load_links(data_dir)?;

    // PHASE 2: Deduplicate, validate and sort events.
    let events = events::load_events(data_dir, &mut identities)?;
    let event_count = events.len();

    println!("\nEVENT PROCESSING COMPLETE");
    println!("  Valid events: {}", events.len());

    // PHASE 3: Create exact 30-minute sessions.
    let sessions = sessions::build_sessions(&events);

    println!("\nSESSIONIZATION COMPLETE");
    println!("  Total sessions: {}", sessions.len());

    // PHASE 4: Load product categories.
    let products = daily::load_products(data_dir)?;

    println!("\nPRODUCT LOOKUP");
    println!("  Products loaded: {}", products.len());

    // PHASE 5: Generate user_daily.
    let user_daily = daily::aggregate_user_daily(&events, &sessions, &products, &identities);
    drop(events);

    println!("\nUSER DAILY AGGREGATION");
    println!("  Daily rows: {}", user_daily.len());

    // PHASE 6: Load, deduplicate and validate clicks.
    let click_data = clicks::load_clicks(data_dir, &mut identities)?;

    println!("\nCLICK PROCESSING COMPLETE");
    println!("  Valid clicks: {}", click_data.clicks.len());
    println!("  Known campaigns: {}", click_data.channels.len());

    // PHASE 7: Generate campaign_daily.
    let campaign_daily = attribution::aggregate_campaign_daily(&sessions, &click_data);

    println!("\nCAMPAIGN ATTRIBUTION COMPLETE");
    println!("  Campaign daily rows: {}", campaign_daily.len());

    // PHASE 8: Verify exact report totals.
    let user_session_count: u64 = user_daily.iter().map(|r| r.sessions).sum();

    let campaign_session_count: u64 = campaign_daily.iter().map(|r| r.sessions).sum();

    let user_event_count: u64 = user_daily.iter().map(|r| r.events).sum();

    let user_revenue_cents: i64 = user_daily.iter().map(|r| r.revenue_cents).sum();

    let campaign_revenue_cents: i64 = campaign_daily.iter().map(|r| r.revenue_cents).sum();

    let session_revenue_cents: i64 = sessions.iter().map(|s| s.revenue_cents).sum();

    assert_eq!(
        user_session_count,
        sessions.len() as u64,
        "User session count mismatch"
    );

    assert_eq!(
        campaign_session_count,
        sessions.len() as u64,
        "Campaign session count mismatch"
    );

    assert_eq!(
        user_event_count, event_count as u64,
        "Daily event count mismatch"
    );

    assert_eq!(
        user_revenue_cents, campaign_revenue_cents,
        "Daily report revenue mismatch"
    );

    assert_eq!(
        user_revenue_cents, session_revenue_cents,
        "Session revenue mismatch"
    );

    println!("\nDATA RECONCILIATION");
    println!("  User sessions: {user_session_count}");
    println!("  Campaign sessions: {campaign_session_count}");
    println!("  Valid events: {user_event_count}");
    println!("  User revenue (cents): {user_revenue_cents}");
    println!("  Campaign revenue (cents): {campaign_revenue_cents}");
    println!("  All checks: PASSED");

    // PHASE 9: Write diagnostic reports.
    // The final competition version must write Parquet.
    let user_path = Path::new("out/user_daily.csv");
    let campaign_path = Path::new("out/campaign_daily.csv");

    daily::write_debug_csv(user_path, &user_daily)?;

    attribution::write_debug_csv(campaign_path, &campaign_daily)?;

    println!("\nDIAGNOSTIC REPORTS");
    println!("  {}", user_path.display());
    println!("  {}", campaign_path.display());

    // Preview campaign attribution.
    println!("\nCAMPAIGN DAILY PREVIEW");

    for row in campaign_daily.iter().take(10) {
        println!(
            "  Campaign={} | Channel={} | Day={} | Sessions={} | Purchasing={} | Revenue={} | Assists={}",
            row.campaign_id,
            row.channel,
            daily::day_to_date(row.utc_day),
            row.sessions,
            row.purchasing_sessions,
            row.revenue_cents,
            row.assisted_sessions,
        );
    }

    println!("\n========================================");
    println!("       RUST PIPELINE COMPLETE");
    println!("========================================");

    println!("  Identity resolution: complete");
    println!("  Event processing: complete");
    println!("  Sessionization: complete");
    println!("  User daily aggregation: complete");
    println!("  Click processing: complete");
    println!("  Campaign attribution: complete");
    println!("  Revenue reconciliation: complete");

    println!("\nNext development stages:");
    println!("  Official fixture verification");
    println!("  Parquet output");
    println!("  Docker integration");
    println!("  Performance optimization");

    Ok(())
}
