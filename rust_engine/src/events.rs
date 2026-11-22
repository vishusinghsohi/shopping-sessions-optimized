use crate::csv_parser::{PARTITION_BUFFER_BYTES, parse_line};
use crate::identity::{IdentityMap, UserKey, UserKeys};
use crate::timestamp::{ParsedTimestamp, parse_timestamp};

use std::collections::{
    HashMap,
    hash_map::{Entry, RandomState},
};
use std::fs::{self, File};
use std::hash::BuildHasher;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    mpsc::{self, Receiver, SyncSender},
};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    View,
    Cart,
    Purchase,
}

#[derive(Debug)]
pub struct Event {
    pub user_id: UserKey,
    pub timestamp_ms: i64,
    pub event_type: EventType,
    pub product_id: Option<Box<str>>,
    pub revenue_cents: i64,
}

struct MeasuredFile {
    file: File,
    read_time: Duration,
}

impl Read for MeasuredFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let start = Instant::now();
        let bytes = self.file.read(buffer);
        self.read_time += start.elapsed();
        bytes
    }
}

#[derive(Default)]
struct Candidates {
    by_id: HashMap<Box<[u8]>, (i64, usize)>,
    events: Vec<Option<Event>>,
}

impl Candidates {
    fn choose_earliest(
        &mut self,
        fields: &[&[u8]; 7],
        links: &IdentityMap,
        users: &mut UserKeys,
        window: (i64, i64),
    ) {
        if fields[0].is_empty() {
            return;
        }

        let Some(ingest_ms) = millis(fields[3]) else {
            return;
        };

        // Keep the earliest ingest time even if that candidate fails validation.
        // Equal-ingest copies retain the first row in file order.
        match self.by_id.entry(Box::from(fields[0])) {
            Entry::Vacant(entry) => {
                entry.insert((ingest_ms, self.events.len()));
                self.events
                    .push(validate(fields, ingest_ms, links, users, window));
            }
            Entry::Occupied(mut entry) if ingest_ms < entry.get().0 => {
                let (previous_ingest_ms, index) = entry.get_mut();
                *previous_ingest_ms = ingest_ms;
                self.events[*index] = validate(fields, ingest_ms, links, users, window);
            }
            Entry::Occupied(_) => {}
        }
    }

    fn into_events(self) -> Vec<Event> {
        drop(self.by_id);
        self.events.into_iter().flatten().collect()
    }
}

// Convert supported timestamp formats into exact
// integer milliseconds.
fn millis(raw: &[u8]) -> Option<i64> {
    match parse_timestamp(raw)? {
        ParsedTimestamp::Millis(value) => Some(value),
        ParsedTimestamp::BeyondI64 => None,
    }
}

// Validate exact integer prices in cents.
fn price_cents(raw: &[u8]) -> Option<i64> {
    if raw.is_empty() {
        return None;
    }

    let mut value = 0i64;

    for &digit in raw {
        if !digit.is_ascii_digit() {
            return None;
        }

        value = value.checked_mul(10)?.checked_add((digit - b'0') as i64)?;

        if value > 10_000_000 {
            return None;
        }
    }

    (value > 0).then_some(value)
}

fn validate(
    fields: &[&[u8]; 7],
    ingest_ms: i64,
    links: &IdentityMap,
    users: &mut UserKeys,
    (window_start, window_end): (i64, i64),
) -> Option<Event> {
    let user = std::str::from_utf8(fields[1]).ok()?;

    if user.is_empty() {
        return None;
    }

    let timestamp_ms = millis(fields[2])?;

    // [window_start, window_end)
    // Start is inclusive, end is exclusive.
    if timestamp_ms < window_start || timestamp_ms >= window_end {
        return None;
    }

    // Exactly 24 hours late is valid.
    // More than 24 hours late is invalid.
    //
    // i128 avoids integer overflow in subtraction.
    let lateness = ingest_ms as i128 - timestamp_ms as i128;

    if lateness > 86_400_000 {
        return None;
    }

    // Case-sensitive event types.
    let event_type = match fields[4] {
        b"view" => EventType::View,
        b"cart" => EventType::Cart,
        b"purchase" => EventType::Purchase,
        _ => return None,
    };

    let product = std::str::from_utf8(fields[5]).ok()?;

    if product.is_empty() {
        return None;
    }

    // Price is relevant only for purchases.
    let revenue_cents = match event_type {
        EventType::Purchase => price_cents(fields[6])?,
        EventType::View | EventType::Cart => 0,
    };

    // Apply the identity map after validation.
    let canonical_user = users.intern(links.canonical(user));

    Some(Event {
        user_id: canonical_user,
        timestamp_ms,
        event_type,
        product_id: (event_type == EventType::Purchase).then(|| Box::from(product)),
        revenue_cents,
    })
}

const BATCH_BYTES: usize = 256 * 1024;
const QUEUED_BATCHES: usize = 2;

struct EventShard {
    structural_rows: usize,
    malformed_rows: usize,
    unique_events: usize,
    names: Vec<Arc<str>>,
    events: Vec<Event>,
}

fn load_shard(input: Receiver<Vec<u8>>, links: &IdentityMap, window: (i64, i64)) -> EventShard {
    let mut users = UserKeys::default();
    let mut candidates = Candidates::default();
    let mut structural_rows = 0;
    let mut malformed_rows = 0;
    for batch in input {
        // The producer appends one LF per physical row, including the last row.
        for row in batch[..batch.len() - 1].split(|&byte| byte == b'\n') {
            if let Some(fields) = parse_line::<7>(row) {
                structural_rows += 1;
                candidates.choose_earliest(&fields, links, &mut users, window);
            } else {
                malformed_rows += 1;
            }
        }
    }
    EventShard {
        structural_rows,
        malformed_rows,
        unique_events: candidates.by_id.len(),
        names: users.into_names(),
        events: candidates.into_events(),
    }
}

fn routing_id(row: &[u8]) -> &[u8] {
    let field = row.split(|&byte| byte == b',').next().unwrap_or_default();
    // Structural validation happens in the worker. For valid rows this must
    // produce the exact same bytes as the CSV parser's first field.
    field
        .strip_prefix(b"\"")
        .and_then(|inner| inner.strip_suffix(b"\""))
        .unwrap_or(field)
}

fn dispatch(files: Vec<PathBuf>, senders: &[SyncSender<Vec<u8>>]) -> io::Result<Duration> {
    let route = RandomState::new();
    let mut batches = vec![Vec::new(); senders.len()];
    let mut read_time = Duration::ZERO;
    let mut line = Vec::new();
    for path in files {
        let file = MeasuredFile {
            file: File::open(path)?,
            read_time: Duration::ZERO,
        };
        let mut reader = BufReader::with_capacity(PARTITION_BUFFER_BYTES, file);
        line.clear();
        reader.read_until(b'\n', &mut line)?;

        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            let row = line.strip_suffix(b"\n").unwrap_or(&line);
            let shard = (route.hash_one(routing_id(row)) % senders.len() as u64) as usize;
            let batch = &mut batches[shard];
            batch.extend_from_slice(row);
            batch.push(b'\n');
            if batch.len() >= BATCH_BYTES {
                senders[shard]
                    .send(std::mem::take(batch))
                    .map_err(|_| io::Error::other("event shard stopped"))?;
            }
        }
        read_time += reader.into_inner().read_time;
    }
    for (batch, sender) in batches.into_iter().zip(senders) {
        if !batch.is_empty() {
            sender
                .send(batch)
                .map_err(|_| io::Error::other("event shard stopped"))?;
        }
    }
    Ok(read_time)
}

fn scan_shards(
    files: Vec<PathBuf>,
    links: &IdentityMap,
    window: (i64, i64),
    workers: usize,
) -> io::Result<(Vec<EventShard>, Duration)> {
    thread::scope(|scope| {
        let mut senders = Vec::with_capacity(workers);
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let (sender, receiver) = mpsc::sync_channel(QUEUED_BATCHES);
            senders.push(sender);
            handles.push(scope.spawn(move || load_shard(receiver, links, window)));
        }

        // A single producer visits partitions in file order. The FIFO queue
        // preserves that order for every copy of an event ID.
        let result = dispatch(files, &senders);
        drop(senders);
        let shards = handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| io::Error::other("event shard panicked"))
            })
            .collect::<io::Result<Vec<_>>>()?;
        Ok((shards, result?))
    })
}

// Read each dataset's exact UTC window.
fn manifest_window(data_dir: &Path) -> io::Result<(i64, i64)> {
    let path = data_dir.join("manifest.json");
    let content = fs::read(path)?;

    let manifest: serde_json::Value = serde_json::from_slice(&content).map_err(io::Error::other)?;

    let bound = |key: &str| -> io::Result<i64> {
        let value = manifest[key].as_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Missing manifest field: {key}"),
            )
        })?;

        millis(value.as_bytes()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid timestamp in manifest: {key}"),
            )
        })
    };

    let start = bound("window_start")?;
    let end = bound("window_end")?;

    if start >= end {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Manifest window_end must be after window_start",
        ));
    }

    Ok((start, end))
}

// Main event-loading function.
pub fn load_events(data_dir: &Path, identities: &mut IdentityMap) -> io::Result<Vec<Event>> {
    let window = manifest_window(data_dir)?;

    let mut files = fs::read_dir(data_dir.join("events"))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<io::Result<Vec<_>>>()?;

    // Read all CSV partitions.
    files.retain(|p| p.extension().is_some_and(|e| e == "csv"));

    files.sort();

    println!("\nEVENT INGESTION");
    println!("  Event CSV files: {}", files.len());
    let available = thread::available_parallelism().map_or(1, |count| count.get());
    let requested = std::env::var("NCPU")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(available);
    let workers = requested.min(available).saturating_sub(1).clamp(1, 7);
    println!("  Event deduplication workers: {workers}");
    let scan_start = Instant::now();
    let (shards, read_time) = scan_shards(files, identities, window, workers)?;

    println!("[INNER] event_read_seconds {:.6}", read_time.as_secs_f64());
    println!(
        "[INNER] event_scan_seconds {:.6}",
        scan_start.elapsed().as_secs_f64()
    );
    let structural_rows: usize = shards.iter().map(|shard| shard.structural_rows).sum();
    let malformed_rows: usize = shards.iter().map(|shard| shard.malformed_rows).sum();
    let unique_events: usize = shards.iter().map(|shard| shard.unique_events).sum();

    println!("  Structurally valid rows: {structural_rows}");
    println!("  Malformed rows: {malformed_rows}");
    println!("  Unique event IDs: {unique_events}");

    let flatten_start = Instant::now();
    let mut events = Vec::with_capacity(shards.iter().map(|shard| shard.events.len()).sum());
    for shard in shards {
        let user_keys: Vec<_> = shard
            .names
            .iter()
            .map(|name| identities.users.intern(name))
            .collect();
        events.extend(shard.events.into_iter().map(|mut event| {
            event.user_id = user_keys[event.user_id.0];
            event
        }));
    }
    println!(
        "[INNER] event_finalize_seconds {:.6}",
        flatten_start.elapsed().as_secs_f64()
    );

    // Both event and click ordering use the same canonical user keys.
    let sort_start = Instant::now();
    events.sort_unstable_by_key(|event| (event.user_id, event.timestamp_ms));
    println!(
        "[INNER] event_sort_seconds {:.6}",
        sort_start.elapsed().as_secs_f64()
    );

    let purchase_count = events
        .iter()
        .filter(|event| event.event_type == EventType::Purchase)
        .count();

    let revenue_cents: i64 = events.iter().map(|event| event.revenue_cents).sum();

    println!("\nEVENT VALIDATION");
    println!("  Valid events: {}", events.len());
    println!("  Valid purchases: {purchase_count}");
    println!("  Revenue (cents): {revenue_cents}");

    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_window() -> (i64, i64) {
        (
            millis(b"2026-10-01T00:00:00Z").unwrap(),
            millis(b"2026-10-15T00:00:00Z").unwrap(),
        )
    }

    fn validate_row(raw: &[u8]) -> Option<Event> {
        let fields = parse_line::<7>(raw).unwrap();
        let ingest_ms = millis(fields[3]).unwrap();
        validate(
            &fields,
            ingest_ms,
            &IdentityMap::new(),
            &mut UserKeys::default(),
            test_window(),
        )
    }

    #[test]
    fn earlier_invalid_duplicate_wins() {
        let later = b"e2,u1,2026-10-03T10:00:00Z,\
2026-10-03T10:05:00Z,view,p1,";

        let earlier = b"e2,u1,2026-10-03 10:00:00,\
2026-10-03T10:01:00Z,view,p1,";

        let later_fields = parse_line::<7>(later).unwrap();
        let earlier_fields = parse_line::<7>(earlier).unwrap();

        for rows in [
            [later_fields, earlier_fields],
            [earlier_fields, later_fields],
        ] {
            let mut candidates = Candidates::default();
            let identities = IdentityMap::new();
            let mut users = UserKeys::default();
            for fields in rows {
                candidates.choose_earliest(&fields, &identities, &mut users, test_window());
            }
            assert_eq!(candidates.by_id.len(), 1);
            assert!(candidates.into_events().is_empty());
        }
    }

    #[test]
    fn quoted_and_unquoted_ids_share_a_route() {
        assert_eq!(routing_id(b"\"e2\",u1,other"), routing_id(b"e2,u1,other"));
        assert_eq!(routing_id(b"\" e2 \",u1,other"), b" e2 ");
        assert_eq!(routing_id(b"e2\r,u1,other"), b"e2\r");
    }

    #[test]
    fn price_validation_is_exact() {
        assert_eq!(price_cents(b"00000125"), Some(125));
        assert_eq!(price_cents(b"10000000"), Some(10_000_000));

        assert_eq!(price_cents(b"10000001"), None);
        assert_eq!(price_cents(b"0"), None);
        assert_eq!(price_cents(b"-125"), None);
        assert_eq!(price_cents(b"12.50"), None);
        assert_eq!(price_cents(b""), None);
        assert_eq!(price_cents("١٢٥".as_bytes()), None);
    }

    #[test]
    fn lateness_boundary_is_exact() {
        let exactly_24h = validate_row(
            b"e1,u1,2026-10-02T00:00:00Z,\
2026-10-03T00:00:00Z,purchase,p1,2500",
        );

        assert!(exactly_24h.is_some());

        let one_second_late = validate_row(
            b"e1,u1,2026-10-02T00:00:00Z,\
2026-10-03T00:00:01Z,purchase,p1,2500",
        );

        assert!(one_second_late.is_none());
    }

    #[test]
    fn enforces_half_open_event_window() {
        let at_start = validate_row(
            b"e1,u1,2026-10-01T00:00:00Z,\
2026-10-01T00:01:00Z,view,p1,",
        );

        assert!(at_start.is_some());

        let at_end = validate_row(
            b"e2,u1,2026-10-15T00:00:00Z,\
2026-10-15T00:01:00Z,view,p1,",
        );

        assert!(at_end.is_none());
    }

    #[test]
    fn views_and_carts_ignore_prices() {
        let view = validate_row(
            b"e1,u1,2026-10-03T10:00:00Z,\
2026-10-03T10:01:00Z,view,p1,invalid",
        );

        assert_eq!(view.unwrap().revenue_cents, 0);

        let cart = validate_row(
            b"e2,u1,2026-10-03T10:00:00Z,\
2026-10-03T10:01:00Z,cart,p1,",
        );

        assert!(cart.is_some());
    }
}
