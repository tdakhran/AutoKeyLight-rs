//! Upcoming meetings from a calendar's iCal (ICS) link, recurring meetings included.
//!
//! Only what's needed to know when meetings start is read: timed events with their start, recurrence rules, excluded
//! dates, and moved or cancelled occurrences of recurring meetings. All day and cancelled events are skipped.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Calendars with years of history can be large
const MAX_CALENDAR_BYTES: u64 = 50 * 1024 * 1024;
/// Meetings are looked up this far ahead, the calendar is reloaded much more often than this
const LOOK_AHEAD: chrono::Duration = chrono::Duration::hours(26);
/// Meetings that started this recently still count, in case the calendar was loaded right after the start
const LOOK_BEHIND: chrono::Duration = chrono::Duration::minutes(5);

#[derive(Clone, Debug)]
pub struct Meeting {
    pub summary: String,
    pub start: DateTime<Utc>,
    /// Identifies this occurrence, so each meeting is only reminded of once
    pub key: String,
}

/// Downloads the calendar and returns the meetings starting soon, sorted by start.
pub fn fetch(url: &str) -> Result<Vec<Meeting>, String> {
    let url = url.trim();
    // Calendar apps hand out webcal:// links, they're served over https
    let url = match url.strip_prefix("webcal://") {
        Some(rest) => format!("https://{rest}"),
        None => url.to_string(),
    };

    // Google's sharing links open the calendar in the browser, they're easy to copy instead of the iCal address
    if url.contains("calendar.google.com") && !url.contains("/ical/") {
        return Err(
            "That's a link to open the calendar in Google Calendar, not its iCal address. Use Settings → your calendar → \
             Integrate calendar → Secret address in iCal format"
                .to_string(),
        );
    }

    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(REQUEST_TIMEOUT)).build().into();
    let calendar = agent
        .get(&url)
        .call()
        .map_err(|error| error.to_string())?
        .body_mut()
        .with_config()
        .limit(MAX_CALENDAR_BYTES)
        .read_to_string()
        .map_err(|error| error.to_string())?;

    if !calendar.contains("BEGIN:VCALENDAR") {
        return Err("The link doesn't point to an iCal calendar".to_string());
    }

    Ok(upcoming_meetings(&calendar, Utc::now()))
}

struct Property {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Property {
    fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }
}

#[derive(Default)]
struct Event {
    uid: String,
    summary: String,
    start: Option<Property>,
    rrules: Vec<String>,
    exdates: Vec<Property>,
    recurrence_id: Option<Property>,
    cancelled: bool,
}

/// Lines starting with a space or tab continue the previous line
fn unfold(calendar: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();

    for line in calendar.lines() {
        match (line.strip_prefix(' ').or_else(|| line.strip_prefix('\t')), lines.last_mut()) {
            (Some(continuation), Some(previous)) => previous.push_str(continuation),
            _ => lines.push(line.to_string()),
        }
    }

    lines
}

/// `NAME;PARAM=value;PARAM="quoted":value`
fn parse_property(line: &str) -> Option<Property> {
    let mut in_quotes = false;
    let colon = line.char_indices().find_map(|(index, c)| {
        if c == '"' {
            in_quotes = !in_quotes;
        }
        (c == ':' && !in_quotes).then_some(index)
    })?;

    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.to_ascii_uppercase();
    let params = parts
        .filter_map(|param| param.split_once('='))
        .map(|(key, value)| (key.to_ascii_uppercase(), value.trim_matches('"').to_string()))
        .collect();

    Some(Property { name, params, value: value.to_string() })
}

fn parse_events(calendar: &str) -> Vec<Event> {
    let mut events = Vec::new();
    let mut current: Option<Event> = None;
    // Alarms and other components nested in an event have properties that mustn't be mixed up with the event's
    let mut nested = 0;

    for line in unfold(calendar) {
        let Some(property) = parse_property(&line) else {
            continue;
        };

        match (property.name.as_str(), property.value.as_str()) {
            ("BEGIN", "VEVENT") => current = Some(Event::default()),
            ("END", "VEVENT") => events.extend(current.take()),
            ("BEGIN", _) if current.is_some() => nested += 1,
            ("END", _) if current.is_some() && nested > 0 => nested -= 1,
            _ => {}
        }

        let Some(event) = current.as_mut().filter(|_| nested == 0) else {
            continue;
        };

        match property.name.as_str() {
            "UID" => event.uid = property.value,
            "SUMMARY" => event.summary = unescape(&property.value),
            "DTSTART" => event.start = Some(property),
            "RRULE" => event.rrules.push(property.value),
            "EXDATE" => event.exdates.push(property),
            "RECURRENCE-ID" => event.recurrence_id = Some(property),
            "STATUS" => event.cancelled = property.value.eq_ignore_ascii_case("CANCELLED"),
            _ => {}
        }
    }

    events
}

fn unescape(text: &str) -> String {
    text.replace("\\n", " ").replace("\\N", " ").replace("\\,", ",").replace("\\;", ";").replace("\\\\", "\\")
}

fn is_all_day(property: &Property) -> bool {
    property.param("VALUE").is_some_and(|value| value.eq_ignore_ascii_case("DATE")) || property.value.len() == 8
}

/// The time zone, if it's one we know. Outlook sometimes uses Windows time zone names, those fall back to local time,
/// which is right for your own meetings in nearly every case.
fn known_time_zone(property: &Property) -> Option<chrono_tz::Tz> {
    property.param("TZID")?.parse().ok()
}

fn to_utc(value: &str, time_zone: Option<chrono_tz::Tz>) -> Option<DateTime<Utc>> {
    if let Some(utc) = value.strip_suffix('Z') {
        return NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S").ok().map(|time| time.and_utc());
    }

    let local = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S")
        .or_else(|_| NaiveDate::parse_from_str(value, "%Y%m%d").map(|date| date.and_hms_opt(0, 0, 0).unwrap()))
        .ok()?;

    match time_zone {
        Some(time_zone) => time_zone.from_local_datetime(&local).earliest().map(|time| time.with_timezone(&Utc)),
        None => Local.from_local_datetime(&local).earliest().map(|time| time.with_timezone(&Utc)),
    }
}

fn property_to_utc(property: &Property) -> Option<DateTime<Utc>> {
    to_utc(&property.value, known_time_zone(property))
}

/// A `DTSTART` or `EXDATE` line for the rrule parser, dropping time zones it doesn't know so they're read as local
fn rrule_line(property: &Property) -> String {
    match known_time_zone(property) {
        Some(time_zone) => format!("{};TZID={}:{}", property.name, time_zone.name(), property.value),
        None => format!("{}:{}", property.name, property.value),
    }
}

/// Start times of a recurring event within `from..until`
fn occurrences(event: &Event, start: &Property, from: DateTime<Utc>, until: DateTime<Utc>) -> Vec<DateTime<Utc>> {
    let mut lines = vec![rrule_line(start)];
    lines.extend(event.rrules.iter().map(|rrule| format!("RRULE:{rrule}")));
    lines.extend(event.exdates.iter().map(rrule_line));

    let Ok(set) = lines.join("\n").parse::<rrule::RRuleSet>() else {
        return Vec::new();
    };

    set.after(from.with_timezone(&rrule::Tz::UTC))
        .before(until.with_timezone(&rrule::Tz::UTC))
        .all(100)
        .dates
        .into_iter()
        .map(|time| time.with_timezone(&Utc))
        .collect()
}

fn upcoming_meetings(calendar: &str, now: DateTime<Utc>) -> Vec<Meeting> {
    let (from, until) = (now - LOOK_BEHIND, now + LOOK_AHEAD);
    let events = parse_events(calendar);

    // Occurrences of recurring meetings that were moved or cancelled, they replace the regular occurrence
    let overridden: HashSet<(String, DateTime<Utc>)> = events
        .iter()
        .filter_map(|event| Some((event.uid.clone(), property_to_utc(event.recurrence_id.as_ref()?)?)))
        .collect();

    let mut meetings: HashMap<String, Meeting> = HashMap::new();

    for event in &events {
        let Some(start) = event.start.as_ref() else {
            continue;
        };
        if event.cancelled || is_all_day(start) {
            continue;
        }

        let starts = if event.rrules.is_empty() || event.recurrence_id.is_some() {
            property_to_utc(start).into_iter().filter(|start| (from..=until).contains(start)).collect()
        } else {
            occurrences(event, start, from, until)
        };

        for start in starts {
            if event.recurrence_id.is_none() && overridden.contains(&(event.uid.clone(), start)) {
                continue;
            }

            let key = format!("{}@{}", event.uid, start.timestamp());
            meetings.insert(key.clone(), Meeting { summary: event.summary.clone(), start, key });
        }
    }

    let mut meetings: Vec<Meeting> = meetings.into_values().collect();
    meetings.sort_by_key(|meeting| meeting.start);
    meetings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(time: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(time).unwrap().with_timezone(&Utc)
    }

    const CALENDAR: &str = "BEGIN:VCALENDAR\r
BEGIN:VEVENT\r
UID:single\r
SUMMARY:One off\r
DTSTART:20261030T090000Z\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:daily\r
SUMMARY:Standup\\, daily\r
DTSTART;TZID=Europe/Berlin:20260101T100000\r
RRULE:FREQ=DAILY\r
EXDATE;TZID=Europe/Berlin:20261031T100000\r
BEGIN:VALARM\r
TRIGGER:-PT10M\r
SUMMARY:Alarm, not the meeting\r
END:VALARM\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:daily\r
SUMMARY:Standup moved\r
RECURRENCE-ID;TZID=Europe/Berlin:20261030T100000\r
DTSTART;TZID=Europe/Berlin:20261030T113000\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:all-day\r
SUMMARY:Holiday\r
DTSTART;VALUE=DATE:20261030\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:cancelled\r
SUMMARY:Cancelled\r
DTSTART:20261030T120000Z\r
STATUS:CANCELLED\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:folded\r
SUMMARY:A very long\r
  title\r
DTSTART:20261030T150000Z\r
END:VEVENT\r
END:VCALENDAR\r
";

    #[test]
    fn finds_upcoming_meetings() {
        let meetings = upcoming_meetings(CALENDAR, at("2026-10-30T08:00:00Z"));
        let found: Vec<(&str, String)> =
            meetings.iter().map(|meeting| (meeting.summary.as_str(), meeting.start.to_rfc3339())).collect();

        assert_eq!(
            found,
            vec![
                ("One off", "2026-10-30T09:00:00+00:00".to_string()),
                // Moved from 10:00 Berlin time (09:00 UTC) to 11:30 (10:30 UTC)
                ("Standup moved", "2026-10-30T10:30:00+00:00".to_string()),
                ("A very long title", "2026-10-30T15:00:00+00:00".to_string()),
                // The 31st is excluded, the 1st is outside the 26 hours
            ]
        );
    }

    #[test]
    fn recurring_meetings_follow_their_time_zone() {
        // Daylight saving time ended in Berlin on October 25th, 10:00 is 09:00 UTC from then on
        let meetings = upcoming_meetings(CALENDAR, at("2026-11-02T08:00:00Z"));
        assert_eq!(meetings[0].summary, "Standup, daily");
        assert_eq!(meetings[0].start, at("2026-11-02T09:00:00Z"));
    }
}
