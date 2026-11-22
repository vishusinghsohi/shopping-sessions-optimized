use crate::csv_parser::parse_line;
use crate::events::Event;
use crate::identity::{IdentityMap, UserKey};
use crate::sessions::Session;

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;

#[derive(Debug)]
pub struct UserDaily {
    pub user_id: String,
    pub utc_day: i64,
    pub sessions: u64,
    pub events: u64,
    pub revenue_cents: i64,
    pub top_category: Option<String>,
}

#[derive(Default)]
struct DailyBuilder {
    sessions: u64,
    events: u64,
    revenue_cents: i64,
    category_revenue: HashMap<String, i64>,
}

// =====================================================
// LOAD PRODUCT CATEGORIES
// =====================================================

pub fn load_products(data_dir: &Path) -> io::Result<HashMap<String, String>> {
    let file = File::open(data_dir.join("products.csv"))?;
    let mut reader = BufReader::new(file);

    let mut line = Vec::new();
    let mut products = HashMap::new();

    // Skip exactly one physical header.
    reader.read_until(b'\n', &mut line)?;

    loop {
        line.clear();

        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }

        // Remove LF only; preserve CR as data.
        if line.last() == Some(&b'\n') {
            line.pop();
        }

        let fields = parse_line::<2>(&line).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "Malformed products.csv row")
        })?;

        let product_id = std::str::from_utf8(fields[0]).map_err(io::Error::other)?;

        let category = std::str::from_utf8(fields[1]).map_err(io::Error::other)?;

        products.insert(product_id.to_owned(), category.to_owned());
    }

    Ok(products)
}

// =====================================================
// USER DAILY AGGREGATION
// =====================================================

pub fn aggregate_user_daily(
    events: &[Event],
    sessions: &[Session],
    products: &HashMap<String, String>,
    identities: &IdentityMap,
) -> Vec<UserDaily> {
    let mut grouped: HashMap<(UserKey, i64), DailyBuilder> = HashMap::new();

    for session in sessions {
        let key = (session.user_id, session.utc_day);

        let daily = grouped.entry(key).or_default();

        daily.sessions += 1;
        daily.events += session.event_count;

        daily.revenue_cents = daily
            .revenue_cents
            .checked_add(session.revenue_cents)
            .expect("Daily revenue overflow");

        // All purchases belong to the UTC day
        // of their session's FIRST event.
        for event in &events[session.start_index..session.end_index] {
            let Some(product_id) = event.product_id.as_deref() else {
                continue;
            };

            // Unknown product IDs use "unknown".
            let category = products
                .get(product_id)
                .map(String::as_str)
                .unwrap_or("unknown");

            let total = daily
                .category_revenue
                .entry(category.to_owned())
                .or_insert(0);

            *total = total
                .checked_add(event.revenue_cents)
                .expect("Category revenue overflow");
        }
    }

    let mut rows = Vec::with_capacity(grouped.len());

    for ((user_id, utc_day), daily) in grouped {
        // Highest purchase revenue wins.
        // Equal revenue: smallest UTF-8 byte-wise
        // category name wins.
        let top_category = daily
            .category_revenue
            .into_iter()
            .max_by(|(name_a, revenue_a), (name_b, revenue_b)| {
                revenue_a
                    .cmp(revenue_b)
                    .then_with(|| name_b.as_bytes().cmp(name_a.as_bytes()))
            })
            .map(|(category, _)| category);

        rows.push(UserDaily {
            user_id: identities.user_name(user_id).to_owned(),
            utc_day,
            sessions: daily.sessions,
            events: daily.events,
            revenue_cents: daily.revenue_cents,
            top_category,
        });
    }

    rows.sort_unstable_by(|a, b| {
        a.user_id
            .as_bytes()
            .cmp(b.user_id.as_bytes())
            .then(a.utc_day.cmp(&b.utc_day))
    });

    rows
}

// =====================================================
// CONVERT UTC DAY TO YYYY-MM-DD
// =====================================================

pub fn day_to_date(utc_day: i64) -> String {
    let z = utc_day + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;

    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;

    let mut year = yoe + era * 400;

    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);

    let mp = (5 * doy + 2) / 153;

    let day = doy - (153 * mp + 2) / 5 + 1;

    let month = mp + if mp < 10 { 3 } else { -9 };

    if month <= 2 {
        year += 1;
    }

    format!("{year:04}-{month:02}-{day:02}")
}

// =====================================================
// WRITE DEBUG CSV
// =====================================================
//
// This is for local verification only.
// Competition output must be Parquet with DATE type.

pub fn write_debug_csv(path: &Path, rows: &[UserDaily]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    writeln!(
        writer,
        "user_id,day,sessions,events,revenue_cents,top_category"
    )?;

    for row in rows {
        writeln!(
            writer,
            "{},{},{},{},{},{}",
            row.user_id,
            day_to_date(row.utc_day),
            row.sessions,
            row.events,
            row.revenue_cents,
            row.top_category.as_deref().unwrap_or("")
        )?;
    }

    writer.flush()
}

// =====================================================
// UNIT TESTS
// =====================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventType;

    fn event(ts: i64, kind: EventType, product: &str, cents: i64) -> Event {
        Event {
            user_id: UserKey(0),
            timestamp_ms: ts,
            event_type: kind,
            product_id: (kind == EventType::Purchase).then(|| Box::from(product)),
            revenue_cents: cents,
        }
    }

    fn test_identities() -> IdentityMap {
        let mut identities = IdentityMap::new();
        assert_eq!(identities.intern_user("u1"), UserKey(0));
        identities
    }

    #[test]
    fn converts_utc_days_to_dates() {
        assert_eq!(day_to_date(0), "1970-01-01");

        let day = 1_791_021_600_000i64.div_euclid(86_400_000);

        assert_eq!(day_to_date(day), "2026-10-03");
    }

    #[test]
    fn category_tie_chooses_smallest_name() {
        let events = vec![
            event(0, EventType::Purchase, "p1", 500),
            event(300_000, EventType::Purchase, "p2", 500),
        ];

        let sessions = crate::sessions::build_sessions(&events);

        let products = HashMap::from([
            ("p1".to_owned(), "shoes".to_owned()),
            ("p2".to_owned(), "books".to_owned()),
        ]);

        let daily = aggregate_user_daily(&events, &sessions, &products, &test_identities());

        assert_eq!(daily.len(), 1);
        assert_eq!(daily[0].sessions, 1);
        assert_eq!(daily[0].events, 2);
        assert_eq!(daily[0].revenue_cents, 1000);

        assert_eq!(daily[0].top_category.as_deref(), Some("books"));
    }

    #[test]
    fn unknown_category_can_win() {
        let events = vec![
            event(0, EventType::Purchase, "missing", 2000),
            event(300_000, EventType::Purchase, "p1", 1500),
        ];

        let sessions = crate::sessions::build_sessions(&events);

        let products = HashMap::from([("p1".to_owned(), "shoes".to_owned())]);

        let daily = aggregate_user_daily(&events, &sessions, &products, &test_identities());

        assert_eq!(daily.len(), 1);
        assert_eq!(daily[0].revenue_cents, 3500);

        assert_eq!(daily[0].top_category.as_deref(), Some("unknown"));
    }

    #[test]
    fn no_purchase_means_null_top_category() {
        let events = vec![event(0, EventType::View, "p1", 0)];

        let sessions = crate::sessions::build_sessions(&events);

        let daily = aggregate_user_daily(&events, &sessions, &HashMap::new(), &test_identities());

        assert_eq!(daily.len(), 1);
        assert_eq!(daily[0].events, 1);
        assert_eq!(daily[0].revenue_cents, 0);
        assert_eq!(daily[0].top_category, None);
    }

    #[test]
    fn crossing_midnight_uses_session_start_day() {
        let day = 86_400_000i64;

        let events = vec![
            event(day - 600_000, EventType::Purchase, "p1", 300),
            event(day + 300_000, EventType::View, "p1", 0),
            event(day + 7_200_000, EventType::View, "p1", 0),
        ];

        let sessions = crate::sessions::build_sessions(&events);

        assert_eq!(sessions.len(), 2);

        let daily = aggregate_user_daily(&events, &sessions, &HashMap::new(), &test_identities());

        assert_eq!(daily.len(), 2);

        assert_eq!(daily[0].utc_day, 0);
        assert_eq!(daily[0].events, 2);
        assert_eq!(daily[0].revenue_cents, 300);

        assert_eq!(daily[1].utc_day, 1);
        assert_eq!(daily[1].events, 1);
        assert_eq!(daily[1].revenue_cents, 0);
    }

    #[test]
    fn aggregates_multiple_sessions_on_same_day() {
        let events = vec![
            event(0, EventType::Purchase, "p1", 300),
            event(3_600_000, EventType::Purchase, "p2", 500),
        ];

        let sessions = crate::sessions::build_sessions(&events);

        assert_eq!(sessions.len(), 2);

        let products = HashMap::from([
            ("p1".to_owned(), "shoes".to_owned()),
            ("p2".to_owned(), "books".to_owned()),
        ]);

        let daily = aggregate_user_daily(&events, &sessions, &products, &test_identities());

        assert_eq!(daily.len(), 1);
        assert_eq!(daily[0].sessions, 2);
        assert_eq!(daily[0].events, 2);
        assert_eq!(daily[0].revenue_cents, 800);

        assert_eq!(daily[0].top_category.as_deref(), Some("books"));
    }
}
