//! Upcoming meetings from a calendar's iCal (ICS) link, recurring meetings included.
//!
//! Only what's needed to know when meetings start is read: timed events with their start, recurrence rules, excluded
//! dates, and moved or cancelled occurrences of recurring meetings. All day and cancelled events are skipped.
//!
//! Calendars with years of history can be megabytes, so the download is read line by line and each event is dropped
//! as soon as it's read. Only meetings within the window the caller asks for are kept. When none of them is still to
//! come, the first one after the window is kept instead, so the next meeting is known even when it's next week.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read};
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Calendars with years of history can be large
const MAX_CALENDAR_BYTES: u64 = 50 * 1024 * 1024;
/// Meetings that started this recently still count, in case the calendar was loaded right after the start
const LOOK_BEHIND: chrono::Duration = chrono::Duration::minutes(5);
/// How far ahead the next meeting is looked for, when there's none within the window
const NEXT_MEETING_HORIZON: chrono::Duration = chrono::Duration::days(366);
/// Meetings after the window that are kept until the end, in case the earliest ones turn out to be cancelled
const LATER_CANDIDATES: usize = 8;
/// Longer lines are cut, only descriptions and attachments get this long and neither is needed
const MAX_LINE_BYTES: usize = 16 * 1024;
const READ_BUFFER_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug)]
pub struct Meeting {
    pub summary: String,
    pub start: DateTime<Utc>,
    /// Identifies this occurrence, so each meeting is only reminded of once
    pub key: String,
}

/// Downloads the calendar and returns the meetings starting within `window`, or the first one after it when none of them
/// is still to come, sorted by start.
pub fn fetch(url: &str, window: chrono::Duration) -> Result<Vec<Meeting>, String> {
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
    let response = agent.get(&url).call().map_err(|error| error.to_string())?;
    let body = response.into_body().into_with_config().limit(MAX_CALENDAR_BYTES).reader();

    upcoming_meetings(BufReader::with_capacity(READ_BUFFER_BYTES, body), Utc::now(), window)
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

/// Reads the calendar's lines one at a time. Lines starting with a space or tab continue the previous line, they're
/// joined.
struct Lines<R> {
    reader: R,
    /// The next line, read ahead to know whether it continues the current one
    next: Vec<u8>,
    done: bool,
}

impl<R: BufRead> Lines<R> {
    fn new(reader: R) -> Self {
        Self { reader, next: Vec::new(), done: false }
    }

    /// Reads a physical line into `buffer`, without its line ending. `false` once there are no more
    fn read_physical(&mut self, buffer: &mut Vec<u8>) -> Result<bool, String> {
        buffer.clear();
        let read = (&mut self.reader)
            .take(MAX_LINE_BYTES as u64)
            .read_until(b'\n', buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Ok(false);
        }
        // Cut off the rest of a line that's too long
        if read == MAX_LINE_BYTES && buffer.last() != Some(&b'\n') {
            self.reader.skip_until(b'\n').map_err(|error| error.to_string())?;
        }

        while buffer.last().is_some_and(|&byte| byte == b'\n' || byte == b'\r') {
            buffer.pop();
        }
        Ok(true)
    }

    fn next_line(&mut self) -> Result<Option<String>, String> {
        let mut line = std::mem::take(&mut self.next);
        if line.is_empty() && !self.done && !self.read_physical(&mut line)? {
            self.done = true;
        }
        if self.done && line.is_empty() {
            return Ok(None);
        }

        let mut next = Vec::new();
        loop {
            if !self.read_physical(&mut next)? {
                self.done = true;
                break;
            }
            match next.first() {
                Some(b' ' | b'\t') => {
                    let room = MAX_LINE_BYTES.saturating_sub(line.len());
                    line.extend_from_slice(&next[1..next.len().min(room + 1)]);
                }
                _ => {
                    self.next = next;
                    break;
                }
            }
        }

        Ok(Some(String::from_utf8_lossy(&line).into_owned()))
    }
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

/// A meeting occurrence that may still be dropped once the whole calendar is read
struct Candidate {
    uid: String,
    summary: String,
    start: DateTime<Utc>,
    /// A moved occurrence of a recurring meeting, it replaces the regular one rather than being replaced
    is_override: bool,
}

/// Keeps what's needed from each event as the calendar is read
struct Collector {
    now: DateTime<Utc>,
    from: DateTime<Utc>,
    window_end: DateTime<Utc>,
    horizon: DateTime<Utc>,
    /// Meetings within `from..=window_end`
    in_window: Vec<Candidate>,
    /// The earliest meetings after the window, sorted by start, at most `LATER_CANDIDATES`
    later: Vec<Candidate>,
    /// Occurrences of recurring meetings that were moved or cancelled, by uid and original start. Only those that
    /// could replace a kept meeting are remembered
    overridden: HashSet<(String, i64)>,
}

impl Collector {
    fn new(now: DateTime<Utc>, window: chrono::Duration) -> Self {
        Self {
            now,
            from: now - LOOK_BEHIND,
            window_end: now + window,
            horizon: now + NEXT_MEETING_HORIZON,
            in_window: Vec::new(),
            later: Vec::new(),
            overridden: HashSet::new(),
        }
    }

    /// Meetings after this can't be kept anymore
    fn latest_kept(&self) -> DateTime<Utc> {
        match self.later.get(LATER_CANDIDATES - 1) {
            Some(last) => last.start,
            None => self.horizon,
        }
    }

    fn add_candidate(&mut self, candidate: Candidate) {
        if candidate.start < self.from || candidate.start > self.horizon {
            return;
        }
        if candidate.start <= self.window_end {
            self.in_window.push(candidate);
            return;
        }

        let position = self.later.partition_point(|later| later.start <= candidate.start);
        if position < LATER_CANDIDATES {
            self.later.insert(position, candidate);
            self.later.truncate(LATER_CANDIDATES);
        }
    }

    fn add_event(&mut self, event: Event) {
        if let Some(original) = event.recurrence_id.as_ref().and_then(property_to_utc) {
            if (self.from..=self.latest_kept()).contains(&original) {
                self.overridden.insert((event.uid.clone(), original.timestamp()));
            }
        }

        let Some(start) = event.start.as_ref() else {
            return;
        };
        if event.cancelled || is_all_day(start) {
            return;
        }

        let is_override = event.recurrence_id.is_some();
        let starts = if event.rrules.is_empty() || is_override {
            property_to_utc(start).into_iter().collect()
        } else {
            self.occurrences(&event, start)
        };

        for start in starts {
            self.add_candidate(Candidate { uid: event.uid.clone(), summary: event.summary.clone(), start, is_override });
        }
    }

    /// Start times of a recurring event within the window, and the first few after it
    fn occurrences(&self, event: &Event, start: &Property) -> Vec<DateTime<Utc>> {
        let mut lines = vec![rrule_line(start)];
        lines.extend(event.rrules.iter().map(|rrule| format!("RRULE:{rrule}")));
        lines.extend(event.exdates.iter().map(rrule_line));

        let Ok(set) = lines.join("\n").parse::<rrule::RRuleSet>() else {
            return Vec::new();
        };

        let utc = |time: DateTime<Utc>| time.with_timezone(&rrule::Tz::UTC);
        let in_window = set.clone().after(utc(self.from)).before(utc(self.window_end)).all(100).dates;
        // Several, in case some of them were cancelled
        let later = set.after(utc(self.window_end)).before(utc(self.latest_kept())).all(LATER_CANDIDATES as u16).dates;

        in_window
            .into_iter()
            .chain(later)
            .map(|time| time.with_timezone(&Utc))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn finish(self) -> Vec<Meeting> {
        let overridden = self.overridden;
        let keep = |candidate: &Candidate| {
            candidate.is_override || !overridden.contains(&(candidate.uid.clone(), candidate.start.timestamp()))
        };

        let in_window: Vec<Candidate> = self.in_window.into_iter().filter(keep).collect();
        // Meetings that started a moment ago are still reminded of, but aren't to come
        let none_to_come = !in_window.iter().any(|candidate| candidate.start > self.now);
        // The next meeting then, so it's known even when it's days away
        let next = self.later.into_iter().filter(keep).take(usize::from(none_to_come));

        let mut meetings: Vec<Meeting> = in_window
            .into_iter()
            .chain(next)
            .map(|candidate| Meeting {
                key: format!("{}@{}", candidate.uid, candidate.start.timestamp()),
                summary: candidate.summary,
                start: candidate.start,
            })
            .collect();

        meetings.sort_by_key(|meeting| meeting.start);
        // The same occurrence can be listed twice, for example in a calendar that was merged from two
        meetings.dedup_by(|a, b| a.key == b.key);
        meetings
    }
}

/// Meetings starting within `window` of `now`, or the first one after it when none of them is still to come, sorted by
/// start
fn upcoming_meetings(reader: impl BufRead, now: DateTime<Utc>, window: chrono::Duration) -> Result<Vec<Meeting>, String> {
    let mut lines = Lines::new(reader);
    let mut collector = Collector::new(now, window);
    let mut is_calendar = false;
    let mut current: Option<Event> = None;
    // Alarms and other components nested in an event have properties that mustn't be mixed up with the event's
    let mut nested = 0;

    while let Some(line) = lines.next_line()? {
        if line.trim().is_empty() {
            continue;
        }
        let property = parse_property(&line);

        // Stop right away when the link is a web page rather than a calendar, instead of reading all of it
        if !is_calendar {
            match &property {
                Some(property) if property.name == "BEGIN" && property.value.eq_ignore_ascii_case("VCALENDAR") => {
                    is_calendar = true;
                    continue;
                }
                _ => return Err("The link doesn't point to an iCal calendar".to_string()),
            }
        }

        let Some(property) = property else {
            continue;
        };

        match (property.name.as_str(), property.value.as_str()) {
            ("BEGIN", "VEVENT") => current = Some(Event::default()),
            ("END", "VEVENT") => {
                if let Some(event) = current.take() {
                    collector.add_event(event);
                }
            }
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

    if !is_calendar {
        return Err("The link doesn't point to an iCal calendar".to_string());
    }

    Ok(collector.finish())
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

    fn meetings(calendar: &str, now: &str, window_minutes: i64) -> Vec<(String, String)> {
        upcoming_meetings(calendar.as_bytes(), at(now), chrono::Duration::minutes(window_minutes))
            .unwrap()
            .into_iter()
            .map(|meeting| (meeting.summary, meeting.start.to_rfc3339()))
            .collect()
    }

    fn found(expected: &[(&str, &str)]) -> Vec<(String, String)> {
        expected.iter().map(|(summary, start)| (summary.to_string(), start.to_string())).collect()
    }

    #[test]
    fn keeps_the_meetings_within_the_window() {
        assert_eq!(
            meetings(CALENDAR, "2026-10-30T08:00:00Z", 8 * 60),
            found(&[
                ("One off", "2026-10-30T09:00:00+00:00"),
                // Moved from 10:00 Berlin time (09:00 UTC) to 11:30 (10:30 UTC)
                ("Standup moved", "2026-10-30T10:30:00+00:00"),
                ("A very long title", "2026-10-30T15:00:00+00:00"),
            ])
        );
        assert_eq!(meetings(CALENDAR, "2026-10-30T08:45:00Z", 30), found(&[("One off", "2026-10-30T09:00:00+00:00")]));
    }

    #[test]
    fn keeps_the_next_meeting_when_none_is_within_the_window() {
        // The 31st is excluded. Daylight saving time ended in Berlin on October 25th, 10:00 is 09:00 UTC
        assert_eq!(
            meetings(CALENDAR, "2026-10-30T16:00:00Z", 30),
            found(&[("Standup, daily", "2026-11-01T09:00:00+00:00")])
        );
    }

    #[test]
    fn meetings_that_just_started_still_count_along_with_the_next() {
        assert_eq!(
            meetings(CALENDAR, "2026-10-30T09:03:00Z", 30),
            found(&[("One off", "2026-10-30T09:00:00+00:00"), ("Standup moved", "2026-10-30T10:30:00+00:00")])
        );
    }

    const WEEKLY: &str = "BEGIN:VCALENDAR\r
BEGIN:VEVENT\r
UID:weekly\r
SUMMARY:Weekly\r
DTSTART:20261002T140000Z\r
RRULE:FREQ=WEEKLY\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:weekly\r
SUMMARY:Weekly\r
RECURRENCE-ID:20261106T140000Z\r
DTSTART:20261106T140000Z\r
STATUS:CANCELLED\r
END:VEVENT\r
END:VCALENDAR\r
";

    #[test]
    fn finds_the_next_meeting_days_ahead_skipping_cancelled_ones() {
        // The 6th is cancelled, the one after is a week later
        assert_eq!(meetings(WEEKLY, "2026-11-02T08:00:00Z", 30), found(&[("Weekly", "2026-11-13T14:00:00+00:00")]));
    }

    #[test]
    fn rejects_web_pages() {
        let page = "<!DOCTYPE html>\n<html>BEGIN:VCALENDAR</html>\n";
        assert!(upcoming_meetings(page.as_bytes(), at("2026-10-30T08:00:00Z"), chrono::Duration::minutes(30)).is_err());
    }

    #[test]
    fn cuts_overlong_lines() {
        let long = "x".repeat(3 * MAX_LINE_BYTES);
        let calendar = CALENDAR.replace("SUMMARY:One off\r\n", &format!("DESCRIPTION:{long}\r\n {long}\r\nSUMMARY:One off\r\n"));
        assert_eq!(meetings(&calendar, "2026-10-30T08:45:00Z", 30), found(&[("One off", "2026-10-30T09:00:00+00:00")]));
    }
}
