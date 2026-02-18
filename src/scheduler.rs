use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use sqlx::SqlitePool;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::pipeline;
use crate::store;

/// RAII guard that removes a channel ID from the in-flight set on drop.
/// Ensures cleanup even if the generation task panics.
struct InFlightGuard {
    set: Arc<Mutex<HashSet<String>>>,
    channel_id: String,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.set.lock().unwrap().remove(&self.channel_id);
    }
}

/// Parsed schedule representation.
///
/// **Note:** `Cron` schedules currently evaluate in UTC, not the user's timezone.
/// Use `at:` or `weekly:` formats for timezone-aware scheduling.
#[derive(Debug, Clone)]
pub enum Schedule {
    /// One or more times per day.
    Daily { times: Vec<NaiveTime> },
    /// Once per week on a specific day and time.
    Weekly { day: Weekday, time: NaiveTime },
    /// Cron expression.
    Cron { schedule: Box<cron::Schedule> },
}

impl Schedule {
    /// Parse a schedule string like "at:08:00,20:00", "weekly:monday,08:00", or "cron:0 8 * * *".
    pub fn parse(s: &str) -> Result<Self> {
        if let Some(times_str) = s.strip_prefix("at:") {
            let mut times = Vec::new();
            for part in times_str.split(',') {
                let t = NaiveTime::parse_from_str(part.trim(), "%H:%M")
                    .with_context(|| format!("invalid time '{}'", part.trim()))?;
                times.push(t);
            }
            times.sort();
            Ok(Schedule::Daily { times })
        } else if let Some(rest) = s.strip_prefix("weekly:") {
            let parts: Vec<&str> = rest.splitn(2, ',').collect();
            if parts.len() != 2 {
                anyhow::bail!("invalid weekly schedule '{s}': expected 'weekly:DAY,HH:MM'");
            }
            let day = parse_weekday(parts[0].trim())?;
            let time = NaiveTime::parse_from_str(parts[1].trim(), "%H:%M")
                .with_context(|| format!("invalid time '{}'", parts[1].trim()))?;
            Ok(Schedule::Weekly { day, time })
        } else if let Some(expr) = s.strip_prefix("cron:") {
            // The cron crate expects 7-field (sec min hour dom mon dow year) expressions.
            // Standard 5-field cron: prepend "0" for seconds, append "*" for year.
            let cron_expr = format!("0 {expr} *");
            let schedule =
                cron::Schedule::from_str(&cron_expr).with_context(|| format!("invalid cron expression '{expr}'"))?;
            Ok(Schedule::Cron {
                schedule: Box::new(schedule),
            })
        } else {
            anyhow::bail!("invalid schedule '{s}': must start with 'at:', 'weekly:', or 'cron:'");
        }
    }

    /// Compute the next tick time after `after`, in the user's timezone.
    ///
    /// Handles DST transitions: if a local time doesn't exist (spring-forward gap),
    /// tries subsequent days rather than returning None.
    pub fn next_tick(&self, tz: Tz, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let after_local = after.with_timezone(&tz);

        match self {
            Schedule::Daily { times } => {
                // Try today and the next 3 days (handles DST gaps)
                let today = after_local.date_naive();
                for day_offset in 0..4i64 {
                    let date = today + chrono::Duration::days(day_offset);
                    for &time in times {
                        if let Some(candidate) = tz.from_local_datetime(&date.and_time(time)).earliest()
                            && candidate > after_local
                        {
                            return Some(candidate.with_timezone(&Utc));
                        }
                        // If earliest() returns None, this time doesn't exist today (DST gap) — skip
                    }
                }
                None
            }
            Schedule::Weekly { day, time } => {
                let today = after_local.date_naive();
                let current_weekday = today.weekday();
                let target_weekday = *day;

                // Days until next occurrence
                let days_ahead =
                    (target_weekday.num_days_from_monday() as i64 - current_weekday.num_days_from_monday() as i64 + 7)
                        % 7;

                // If it's the same day, check if time has passed
                let candidate_date = if days_ahead == 0 {
                    if let Some(candidate) = tz.from_local_datetime(&today.and_time(*time)).earliest()
                        && candidate > after_local
                    {
                        return Some(candidate.with_timezone(&Utc));
                    }
                    // Time passed today or doesn't exist (DST gap) — next week
                    today + chrono::Duration::days(7)
                } else {
                    today + chrono::Duration::days(days_ahead)
                };

                // Try candidate_date, then next week if DST gap
                if let Some(candidate) = tz.from_local_datetime(&candidate_date.and_time(*time)).earliest() {
                    return Some(candidate.with_timezone(&Utc));
                }
                // DST gap on target date — try next week
                let fallback = candidate_date + chrono::Duration::days(7);
                tz.from_local_datetime(&fallback.and_time(*time))
                    .earliest()
                    .map(|c| c.with_timezone(&Utc))
            }
            Schedule::Cron { schedule } => schedule.after(&after).next(),
        }
    }

    /// Check if a generation is due.
    ///
    /// `after` is the reference time to compute the next tick from (typically `last_generated`).
    /// Returns true if the next scheduled tick after `after` is at or before `now`.
    pub fn is_due(&self, tz: Tz, after: DateTime<Utc>, now: DateTime<Utc>) -> bool {
        match self.next_tick(tz, after) {
            Some(next) => next <= now,
            None => false,
        }
    }
}

fn parse_weekday(s: &str) -> Result<Weekday> {
    match s.to_lowercase().as_str() {
        "monday" | "mon" => Ok(Weekday::Mon),
        "tuesday" | "tue" => Ok(Weekday::Tue),
        "wednesday" | "wed" => Ok(Weekday::Wed),
        "thursday" | "thu" => Ok(Weekday::Thu),
        "friday" | "fri" => Ok(Weekday::Fri),
        "saturday" | "sat" => Ok(Weekday::Sat),
        "sunday" | "sun" => Ok(Weekday::Sun),
        _ => anyhow::bail!("unknown weekday '{s}'"),
    }
}

/// Main scheduler loop. Wakes every 30 seconds and checks all enabled channels.
pub async fn scheduler_loop(
    pool: SqlitePool,
    config: Arc<Config>,
    semaphore: Arc<Semaphore>,
    tg_client: Option<grammers_client::Client>,
    cancel: CancellationToken,
) {
    info!("scheduler started");

    // Track which channels have in-flight generations to prevent double-firing
    let in_flight: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

    // Track when we first saw channels that have never generated.
    // For new channels (last_generated = NULL), we wait for their next scheduled tick
    // instead of firing immediately. The first-seen time serves as the reference for
    // computing the next tick. On daemon restart this resets, which is correct —
    // missed ticks are always skipped (see docs/specs/daemon.md "Missed Ticks").
    let mut first_seen: HashMap<String, DateTime<Utc>> = HashMap::new();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("scheduler shutting down");
                return;
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
        }

        let tz: Tz = match config.pail.timezone.parse() {
            Ok(tz) => tz,
            Err(_) => {
                error!(tz = %config.pail.timezone, "invalid timezone in config");
                continue;
            }
        };

        let channels = match store::get_all_enabled_channels(&pool).await {
            Ok(c) => c,
            Err(e) => {
                error!(error = %e, "failed to load channels for scheduling");
                continue;
            }
        };

        let now = Utc::now();

        for channel in &channels {
            // Skip if this channel already has an in-flight generation
            if in_flight.lock().unwrap().contains(&channel.id) {
                debug!(channel = %channel.name, "generation already in progress, skipping");
                continue;
            }

            let schedule_str = match &channel.schedule {
                Some(s) => s,
                None => continue, // no schedule — CLI-only channel
            };
            let schedule = match Schedule::parse(schedule_str) {
                Ok(s) => s,
                Err(e) => {
                    warn!(channel = %channel.name, error = %e, "invalid schedule, skipping");
                    continue;
                }
            };

            // For channels that have never generated, use the time we first saw them
            // as the reference point. They wait for their next scheduled tick rather than
            // firing immediately. The pipeline still uses the 7-day lookback for content
            // collection when last_generated is NULL.
            let after = channel
                .last_generated
                .unwrap_or_else(|| *first_seen.entry(channel.id.clone()).or_insert(now));

            if !schedule.is_due(tz, after, now) {
                continue;
            }

            // Find channel config
            let channel_config = match config.output_channel.iter().find(|c| c.slug == channel.slug) {
                Some(c) => c.clone(),
                None => {
                    warn!(slug = %channel.slug, "channel not found in config, skipping");
                    continue;
                }
            };

            // Mark channel as in-flight (drop guard ensures removal even on panic)
            let channel_id = channel.id.clone();
            in_flight.lock().unwrap().insert(channel_id.clone());

            let pool = pool.clone();
            let config = config.clone();
            let semaphore = semaphore.clone();
            let tg_client = tg_client.clone();
            let cancel = cancel.clone();
            let in_flight = in_flight.clone();

            tokio::spawn(async move {
                // Guard ensures channel is removed from in-flight set on drop (including panic)
                let _guard = InFlightGuard {
                    set: in_flight,
                    channel_id,
                };

                // Acquire semaphore permit (limits concurrent generations)
                let _permit = match semaphore.acquire().await {
                    Ok(p) => p,
                    Err(_) => return,
                };

                if cancel.is_cancelled() {
                    return;
                }

                info!(channel = %channel_config.name, "scheduled generation starting");

                match pipeline::run_generation(&pool, &config, &channel_config, None, false, tg_client.as_ref(), cancel)
                    .await
                {
                    Ok(Some(r)) => {
                        info!(channel = %channel_config.name, title = %r.article.title, "scheduled generation complete");
                    }
                    Ok(None) => {
                        debug!(channel = %channel_config.name, "scheduled generation skipped (no content)");
                    }
                    Err(e) => {
                        error!(channel = %channel_config.name, error = %e, "scheduled generation failed");
                    }
                }
            });
        }
    }
}
