use crate::events::{Event, EventType};
use crate::identity::UserKey;

// Exact competition boundaries in milliseconds.
const SESSION_GAP_MS: i128 = 1_800_000;
const DAY_MS: i64 = 86_400_000;

#[derive(Debug)]
pub struct Session {
    pub user_id: UserKey,
    pub start_ms: i64,
    pub utc_day: i64,

    // Range of events in the sorted event vector.
    // start_index is inclusive.
    // end_index is exclusive.
    pub start_index: usize,
    pub end_index: usize,

    pub event_count: u64,
    pub purchase_count: u64,
    pub revenue_cents: i64,
}

// =====================================================
// SHOPPING SESSIONIZATION
// =====================================================
//
// Input events must already be sorted by:
// 1. Canonical user key
// 2. Event timestamp (milliseconds)
//
// Competition rule:
// A new session begins only if the gap between
// consecutive events is GREATER than 30 minutes.
//
// Exactly 30 minutes belongs to the same session.
// =====================================================

pub fn build_sessions(events: &[Event]) -> Vec<Session> {
    let mut sessions: Vec<Session> = Vec::new();

    for (index, event) in events.iter().enumerate() {
        let start_new_session = match sessions.last() {
            None => true,

            Some(current) => {
                // Different canonical users
                // always have separate sessions.
                if current.user_id != event.user_id {
                    true
                } else {
                    // Compare against the immediately
                    // preceding event, not session start.
                    let previous_timestamp = events[current.end_index - 1].timestamp_ms;

                    // i128 prevents subtraction overflow.
                    let gap = event.timestamp_ms as i128 - previous_timestamp as i128;

                    gap > SESSION_GAP_MS
                }
            }
        };

        if start_new_session {
            sessions.push(Session {
                user_id: event.user_id,
                start_ms: event.timestamp_ms,

                // UTC day of the FIRST event.
                utc_day: event.timestamp_ms.div_euclid(DAY_MS),

                start_index: index,
                end_index: index,

                event_count: 0,
                purchase_count: 0,
                revenue_cents: 0,
            });
        }

        let current = sessions.last_mut().expect("A session must exist");

        // Include the current event.
        current.end_index = index + 1;
        current.event_count += 1;

        if event.event_type == EventType::Purchase {
            current.purchase_count += 1;
        }

        // Integer cents only: no floating-point rounding.
        current.revenue_cents = current
            .revenue_cents
            .checked_add(event.revenue_cents)
            .expect("Session revenue overflow");
    }

    sessions
}

// =====================================================
// UNIT TESTS
// =====================================================

#[cfg(test)]
mod tests {
    use super::*;

    use crate::timestamp::{ParsedTimestamp, parse_timestamp};

    fn sample(
        user_id: UserKey,
        timestamp_ms: i64,
        event_type: EventType,
        revenue_cents: i64,
    ) -> Event {
        Event {
            user_id,
            timestamp_ms,
            event_type,
            product_id: (event_type == EventType::Purchase).then(|| Box::from("p1")),
            revenue_cents,
        }
    }

    fn millis(value: &[u8]) -> i64 {
        match parse_timestamp(value).unwrap() {
            ParsedTimestamp::Millis(ms) => ms,
            ParsedTimestamp::BeyondI64 => {
                panic!("Expected an i64 timestamp")
            }
        }
    }

    // TEST 1:
    // Exactly 30 minutes must remain one session.
    #[test]
    fn exactly_30_minutes_is_same_session() {
        let start = 1_791_021_600_000i64;

        let events = vec![
            sample(UserKey(0), start, EventType::View, 0),
            sample(UserKey(0), start + 1_800_000, EventType::Cart, 0),
        ];

        let sessions = build_sessions(&events);

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].event_count, 2);
    }

    // TEST 2:
    // One millisecond more than 30 minutes
    // must create a new session.
    #[test]
    fn one_millisecond_over_starts_new_session() {
        let start = 1_791_021_600_000i64;

        let events = vec![
            sample(UserKey(0), start, EventType::View, 0),
            sample(UserKey(0), start + 1_800_001, EventType::View, 0),
        ];

        let sessions = build_sessions(&events);

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].event_count, 1);
        assert_eq!(sessions[1].event_count, 1);
    }

    // TEST 3:
    // Compare gaps against the previous event,
    // not against the beginning of the session.
    #[test]
    fn uses_immediately_previous_event() {
        let start = 1_791_021_600_000i64;

        let events = vec![
            sample(UserKey(0), start, EventType::View, 0),
            sample(UserKey(0), start + 1_800_000, EventType::Cart, 0),
            sample(UserKey(0), start + 3_600_000, EventType::Purchase, 2500),
        ];

        let sessions = build_sessions(&events);

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].event_count, 3);
        assert_eq!(sessions[0].purchase_count, 1);
        assert_eq!(sessions[0].revenue_cents, 2500);

        assert_eq!(sessions[0].start_index, 0);
        assert_eq!(sessions[0].end_index, 3);
    }

    // TEST 4:
    // Different users cannot share a session.
    #[test]
    fn different_users_have_separate_sessions() {
        let timestamp = 1_791_021_600_000i64;

        let events = vec![
            sample(UserKey(0), timestamp, EventType::View, 0),
            sample(UserKey(1), timestamp, EventType::View, 0),
        ];

        let sessions = build_sessions(&events);

        assert_eq!(sessions.len(), 2);

        assert_eq!(sessions[0].user_id, UserKey(0));
        assert_eq!(sessions[1].user_id, UserKey(1));
    }

    // TEST 5:
    // A session crossing midnight belongs to
    // the UTC day when the session started.
    #[test]
    fn session_uses_start_day_and_exact_revenue() {
        let before_midnight = millis(b"2026-10-03T23:50:00Z");

        let after_midnight = millis(b"2026-10-04T00:05:00Z");

        let events = vec![
            sample(UserKey(0), before_midnight, EventType::Purchase, 2500),
            sample(UserKey(0), after_midnight, EventType::Purchase, 1750),
        ];

        let sessions = build_sessions(&events);

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].event_count, 2);
        assert_eq!(sessions[0].purchase_count, 2);
        assert_eq!(sessions[0].revenue_cents, 4250);

        assert_eq!(sessions[0].utc_day, before_midnight.div_euclid(DAY_MS));
    }
}
