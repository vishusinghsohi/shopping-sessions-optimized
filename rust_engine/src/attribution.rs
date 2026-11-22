use crate::clicks::ClickData;
use crate::daily::day_to_date;
use crate::sessions::Session;

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::Path;

const SEVEN_DAYS_MS: i128 = 604_800_000;

#[derive(Debug)]
pub struct CampaignDaily {
    pub campaign_id: String,
    pub channel: String,
    pub utc_day: i64,
    pub sessions: u64,
    pub purchasing_sessions: u64,
    pub revenue_cents: i64,
    pub assisted_sessions: u64,
}

#[derive(Default)]
struct Totals {
    sessions: u64,
    purchasing_sessions: u64,
    revenue_cents: i64,
    assisted_sessions: u64,
}

// =====================================================
// CAMPAIGN ATTRIBUTION
// =====================================================
//
// 1. Match clicks and sessions by canonical user.
// 2. Eligible clicks are within the preceding 7 days,
//    including both boundaries.
// 3. Credit the latest eligible click.
// 4. If timestamps tie, smallest click_id wins.
// 5. Every distinct eligible campaign gets one assist.
// 6. No eligible clicks => organic session.
//
// Uses a sliding window instead of scanning every
// click for every session.
// =====================================================

pub fn aggregate_campaign_daily(
    sessions: &[Session],
    click_data: &ClickData,
) -> Vec<CampaignDaily> {
    let clicks = &click_data.clicks;

    let mut grouped: HashMap<(&str, i64), Totals> = HashMap::new();

    let mut session_index = 0usize;
    let mut click_index = 0usize;

    while session_index < sessions.len() {
        let user = sessions[session_index].user_id;

        let session_begin = session_index;

        while session_index < sessions.len() && sessions[session_index].user_id == user {
            session_index += 1;
        }

        let user_sessions = &sessions[session_begin..session_index];

        // Advance to this user's clicks.
        while click_index < clicks.len() && clicks[click_index].user_id < user {
            click_index += 1;
        }

        let click_begin = click_index;

        while click_index < clicks.len() && clicks[click_index].user_id == user {
            click_index += 1;
        }

        let user_clicks = &clicks[click_begin..click_index];

        // Active window [left, right).
        let mut left = 0usize;
        let mut right = 0usize;

        // Number of currently eligible clicks
        // for each campaign.
        let mut active: HashMap<&str, usize> = HashMap::new();

        for session in user_sessions {
            let session_start = session.start_ms;

            let earliest = session_start as i128 - SEVEN_DAYS_MS;

            // Include clicks up to and including
            // the session start timestamp.
            while right < user_clicks.len() && user_clicks[right].timestamp_ms <= session_start {
                let campaign = user_clicks[right].campaign_id.as_ref();

                *active.entry(campaign).or_insert(0) += 1;

                right += 1;
            }

            // Remove clicks outside the 7-day window.
            while left < right && (user_clicks[left].timestamp_ms as i128) < earliest {
                let campaign = user_clicks[left].campaign_id.as_ref();

                let count = active.get_mut(campaign).expect("Campaign must be active");

                *count -= 1;

                if *count == 0 {
                    active.remove(campaign);
                }

                left += 1;
            }

            // Each distinct eligible campaign assists
            // this session exactly once.
            for &campaign in active.keys() {
                let entry = grouped.entry((campaign, session.utc_day)).or_default();

                entry.assisted_sessions += 1;
            }

            // Latest eligible click wins.
            // On equal timestamps, smallest click_id.
            let credited_campaign = if left < right {
                let latest_time = user_clicks[right - 1].timestamp_ms;

                let first_latest = user_clicks[left..right]
                    .partition_point(|click| click.timestamp_ms < latest_time);

                user_clicks[left + first_latest].campaign_id.as_ref()
            } else {
                "organic"
            };

            let entry = grouped
                .entry((credited_campaign, session.utc_day))
                .or_default();

            entry.sessions += 1;

            if session.purchase_count > 0 {
                entry.purchasing_sessions += 1;
            }

            entry.revenue_cents = entry
                .revenue_cents
                .checked_add(session.revenue_cents)
                .expect("Campaign revenue overflow");
        }
    }

    let mut rows = Vec::with_capacity(grouped.len());

    for ((campaign_id, utc_day), totals) in grouped {
        let channel = if campaign_id == "organic" {
            "organic".to_owned()
        } else {
            click_data
                .channels
                .get(campaign_id)
                .expect("Known campaign")
                .clone()
        };

        rows.push(CampaignDaily {
            campaign_id: campaign_id.to_owned(),
            channel,
            utc_day,
            sessions: totals.sessions,
            purchasing_sessions: totals.purchasing_sessions,
            revenue_cents: totals.revenue_cents,
            assisted_sessions: totals.assisted_sessions,
        });
    }

    rows.sort_unstable_by(|a, b| {
        a.campaign_id
            .as_bytes()
            .cmp(b.campaign_id.as_bytes())
            .then(a.utc_day.cmp(&b.utc_day))
    });

    rows
}

// =====================================================
// WRITE DIAGNOSTIC CSV
// =====================================================
//
// This file is for local fixture verification.
// Competition output must eventually be Parquet.
// =====================================================

pub fn write_debug_csv(path: &Path, rows: &[CampaignDaily]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    writeln!(
        writer,
        "campaign_id,channel,day,sessions,purchasing_sessions,revenue_cents,assisted_sessions"
    )?;

    for row in rows {
        writeln!(
            writer,
            "{},{},{},{},{},{},{}",
            row.campaign_id,
            row.channel,
            day_to_date(row.utc_day),
            row.sessions,
            row.purchasing_sessions,
            row.revenue_cents,
            row.assisted_sessions,
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
    use crate::clicks::Click;
    use crate::identity::UserKey;
    use std::sync::Arc;

    const BASE: i64 = 1_791_021_600_000;

    fn session(time: i64, purchases: u64, revenue: i64) -> Session {
        Session {
            user_id: UserKey(0),
            start_ms: time,
            utc_day: time.div_euclid(86_400_000),
            start_index: 0,
            end_index: 0,
            event_count: 1,
            purchase_count: purchases,
            revenue_cents: revenue,
        }
    }

    fn click(id: &str, campaign: &str, time: i64) -> Click {
        Click {
            click_id: Arc::from(id.as_bytes()),
            user_id: UserKey(0),
            campaign_id: Arc::from(campaign),
            timestamp_ms: time,
        }
    }

    fn data(clicks: Vec<Click>) -> ClickData {
        ClickData {
            clicks,
            channels: HashMap::from([
                (Arc::from("c_1"), "search".to_owned()),
                (Arc::from("c_2"), "social".to_owned()),
            ]),
        }
    }

    fn find<'a>(rows: &'a [CampaignDaily], campaign: &str) -> &'a CampaignDaily {
        rows.iter()
            .find(|row| row.campaign_id == campaign)
            .expect("Campaign row should exist")
    }

    #[test]
    fn latest_click_gets_credit() {
        let sessions = vec![session(BASE, 0, 0)];

        let clicks = data(vec![
            click("k1", "c_1", BASE - 7_200_000),
            click("k2", "c_2", BASE - 3_600_000),
        ]);

        let rows = aggregate_campaign_daily(&sessions, &clicks);

        assert_eq!(find(&rows, "c_1").sessions, 0);
        assert_eq!(find(&rows, "c_1").assisted_sessions, 1);
        assert_eq!(find(&rows, "c_2").sessions, 1);
        assert_eq!(find(&rows, "c_2").assisted_sessions, 1);
    }

    #[test]
    fn equal_timestamp_uses_smallest_click_id() {
        let sessions = vec![session(BASE, 0, 0)];

        let clicks = data(vec![
            click("k1", "c_1", BASE - 60_000),
            click("k2", "c_2", BASE - 60_000),
        ]);

        let rows = aggregate_campaign_daily(&sessions, &clicks);

        assert_eq!(find(&rows, "c_1").sessions, 1);
        assert_eq!(find(&rows, "c_2").sessions, 0);
    }

    #[test]
    fn exactly_seven_days_is_eligible() {
        let sessions = vec![session(BASE, 0, 0)];

        let clicks = data(vec![
            click("too_old", "c_2", BASE - 604_800_001),
            click("boundary", "c_1", BASE - 604_800_000),
        ]);

        let rows = aggregate_campaign_daily(&sessions, &clicks);

        assert_eq!(rows.len(), 1);
        assert_eq!(find(&rows, "c_1").sessions, 1);
    }

    #[test]
    fn future_click_is_not_eligible() {
        let sessions = vec![session(BASE, 0, 0)];

        let clicks = data(vec![click("k1", "c_1", BASE + 1)]);

        let rows = aggregate_campaign_daily(&sessions, &clicks);

        assert_eq!(rows.len(), 1);
        assert_eq!(find(&rows, "organic").sessions, 1);
        assert_eq!(find(&rows, "organic").assisted_sessions, 0);
    }

    #[test]
    fn repeated_campaign_assists_only_once() {
        let sessions = vec![session(BASE, 0, 0)];

        let clicks = data(vec![
            click("k0", "c_2", BASE - 200_000),
            click("k1", "c_1", BASE - 100_000),
            click("k2", "c_1", BASE - 50_000),
        ]);

        let rows = aggregate_campaign_daily(&sessions, &clicks);

        assert_eq!(find(&rows, "c_1").assisted_sessions, 1);
        assert_eq!(find(&rows, "c_2").assisted_sessions, 1);
    }

    #[test]
    fn one_click_credits_multiple_sessions() {
        let sessions = vec![session(BASE, 0, 0), session(BASE + 3_600_000, 0, 0)];

        let clicks = data(vec![click("k1", "c_1", BASE - 60_000)]);

        let rows = aggregate_campaign_daily(&sessions, &clicks);

        assert_eq!(find(&rows, "c_1").sessions, 2);
        assert_eq!(find(&rows, "c_1").assisted_sessions, 2);
    }

    #[test]
    fn purchase_and_revenue_are_attributed() {
        let sessions = vec![session(BASE, 2, 5725)];

        let clicks = data(vec![click("k1", "c_1", BASE - 60_000)]);

        let rows = aggregate_campaign_daily(&sessions, &clicks);

        assert_eq!(find(&rows, "c_1").sessions, 1);
        assert_eq!(find(&rows, "c_1").purchasing_sessions, 1);
        assert_eq!(find(&rows, "c_1").revenue_cents, 5725);
    }
}
