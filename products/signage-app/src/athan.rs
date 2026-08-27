#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::float_arithmetic,
    clippy::suboptimal_flops,
    reason = "spherical astronomy is floating-point arithmetic on bounded civil dates"
)]

//! Prayer times for the `athan` kind, computed here from the location the
//! Space stored. The receiver never fetches a timetable; the day and the
//! coordinates are enough.

use std::collections::BTreeMap;

use jiff::{civil::Date, tz::TimeZone, Timestamp, ToSpan, Zoned};

const KIND: &str = "athan";

#[derive(Debug, Clone, Copy, PartialEq)]
struct Method {
    fajr: f64,
    isha: f64,
    maghrib_min: Option<f64>,
    isha_min: Option<f64>,
}

const MWL: Method = Method {
    fajr: 18.0,
    isha: 17.0,
    maghrib_min: None,
    isha_min: None,
};
const ISNA: Method = Method {
    fajr: 15.0,
    isha: 15.0,
    maghrib_min: None,
    isha_min: None,
};
const EGYPT: Method = Method {
    fajr: 19.5,
    isha: 17.5,
    maghrib_min: None,
    isha_min: None,
};
const MAKKAH: Method = Method {
    fajr: 18.5,
    isha: 0.0,
    maghrib_min: None,
    isha_min: Some(90.0),
};
const KARACHI: Method = Method {
    fajr: 18.0,
    isha: 18.0,
    maghrib_min: None,
    isha_min: None,
};
const TEHRAN: Method = Method {
    fajr: 17.7,
    isha: 14.0,
    maghrib_min: None,
    isha_min: None,
};
const JAFARI: Method = Method {
    fajr: 16.0,
    isha: 14.0,
    maghrib_min: Some(4.0),
    isha_min: None,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    pub hour: u8,
    pub minute: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prayer {
    pub name: &'static str,
    pub adhan: Clock,
    pub iqamah: Option<Clock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Ink,
    Paper,
    Emerald,
    Night,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Table,
    Countdown { label: &'static str, remain_s: u32 },
    Silence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayTimes {
    pub prayers: Vec<Prayer>,
    pub next: usize,
    /// The next event is that prayer's iqamah, not its adhan.
    pub next_is_iqamah: bool,
    pub zone: String,
    pub now_label: String,
    pub hijri_label: String,
    pub next_change_unix_ms: u64,
    pub theme: Theme,
    pub clock_24h: bool,
    pub show_iqamah: bool,
    pub show_hijri: bool,
    pub phase: Phase,
}

impl DayTimes {
    pub fn next_event_clock(&self) -> Option<Clock> {
        let prayer = self.prayers.get(self.next)?;
        if self.next_is_iqamah {
            prayer.iqamah
        } else {
            Some(prayer.adhan)
        }
    }
}

pub fn kind_is_athan(kind: &str) -> bool {
    kind == KIND
}

/// Space config wins over a Kind row snapshot.
pub fn overlay(
    space: Option<&BTreeMap<String, String>>,
    entry: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = entry.clone();
    if let Some(space) = space {
        for (key, value) in space {
            out.insert(key.clone(), value.clone());
        }
    }
    out
}

pub fn times_from_settings(
    settings: &BTreeMap<String, String>,
    now_unix_ms: u64,
) -> Option<DayTimes> {
    let lat: f64 = settings.get("latitude")?.parse().ok()?;
    let lng: f64 = settings.get("longitude")?.parse().ok()?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lng) {
        return None;
    }
    let method = parse_method(settings.get("method").map(String::as_str).unwrap_or(""));
    let asr_factor = match settings.get("asr_school").map(String::as_str).unwrap_or("") {
        "hanafi" => 2.0,
        _ => 1.0,
    };
    let zone_name = settings
        .get("timezone")
        .map(String::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or("UTC");
    let zone = TimeZone::get(zone_name).unwrap_or(TimeZone::UTC);
    let now_ms = i64::try_from(now_unix_ms).ok()?;
    let now = Timestamp::from_millisecond(now_ms).ok()?.to_zoned(zone);
    let date = now.date();
    let offset_hours = f64::from(now.offset().seconds()) / 3600.0;
    let hours = compute(lat, lng, offset_hours, date, method, asr_factor);
    // Above roughly 48°, fajr or isha can fail to occur for weeks: the sun
    // never reaches the angle, `sun_angle` answers None, and the whole card
    // used to blank with nothing saying why. Every serious implementation
    // carries a rule for this; refusing to have one is not neutrality, it is
    // a dark screen through a Scottish summer.
    let hours = high_latitude(hours, &method, rule_of(settings));
    let fajr = hours
        .fajr
        .and_then(|value| prayer_row("Fajr", value, settings, "tune_fajr", "iqamah_fajr"))?;
    let maghrib = hours.maghrib.and_then(|value| {
        prayer_row("Maghrib", value, settings, "tune_maghrib", "iqamah_maghrib")
    })?;
    let mut prayers = vec![fajr];
    if flag(settings, "show_sunrise", true) {
        if let Some(row) = hours
            .sunrise
            .and_then(|value| prayer_row("Sunrise", value, settings, "tune_sunrise", ""))
        {
            prayers.push(row);
        }
    }
    if let Some(row) = hours
        .dhuhr
        .and_then(|value| prayer_row("Dhuhr", value, settings, "tune_dhuhr", "iqamah_dhuhr"))
    {
        prayers.push(row);
    }
    if let Some(row) = hours
        .asr
        .and_then(|value| prayer_row("Asr", value, settings, "tune_asr", "iqamah_asr"))
    {
        prayers.push(row);
    }
    prayers.push(maghrib);
    if let Some(row) = hours
        .isha
        .and_then(|value| prayer_row("Isha", value, settings, "tune_isha", "iqamah_isha"))
    {
        prayers.push(row);
    }
    if now.weekday() == jiff::civil::Weekday::Friday {
        apply_jumuah(&mut prayers, settings);
    }
    let current = minutes_of_day(now.hour() as u8, now.minute() as u8);
    let events = event_instants(&prayers, &now, date);
    let (next, next_is_iqamah, phase, next_change_unix_ms) =
        phase_at(now_unix_ms, current, &prayers, &events, settings, &now)?;
    let clock_24h = flag(settings, "clock_24h", true);
    let show_iqamah =
        flag(settings, "show_iqamah", true) && prayers.iter().any(|row| row.iqamah.is_some());
    Some(DayTimes {
        next,
        next_is_iqamah,
        now_label: format_clock(
            Clock {
                hour: now.hour() as u8,
                minute: now.minute() as u8,
            },
            clock_24h,
        ),
        hijri_label: hijri_label(date, int_in(settings, "hijri_offset", 0, -2, 2)),
        zone: zone_name.to_owned(),
        prayers,
        next_change_unix_ms,
        theme: parse_theme(settings.get("theme").map(String::as_str).unwrap_or("")),
        clock_24h,
        show_iqamah,
        show_hijri: flag(settings, "show_hijri", true),
        phase,
    })
}

fn prayer_row(
    name: &'static str,
    hours: f64,
    settings: &BTreeMap<String, String>,
    tune_key: &str,
    iqamah_key: &str,
) -> Option<Prayer> {
    let tuned = hours + f64::from(int_in(settings, tune_key, 0, -30, 30)) / 60.0;
    let adhan = hours_to_clock(tuned)?;
    let iqamah = if iqamah_key.is_empty() {
        None
    } else {
        match settings.get(iqamah_key).map(String::as_str).unwrap_or("") {
            "" => None,
            raw => {
                let offset = clamp_i32(raw.parse().ok()?, 0, 180);
                Some(add_minutes(adhan, offset)?)
            }
        }
    };
    Some(Prayer {
        name,
        adhan,
        iqamah,
    })
}

fn apply_jumuah(prayers: &mut [Prayer], settings: &BTreeMap<String, String>) {
    let Some(dhuhr) = prayers.iter_mut().find(|row| row.name == "Dhuhr") else {
        return;
    };
    if let Some(clock) = parse_hhmm(
        settings
            .get("jumuah_khutbah")
            .map(String::as_str)
            .unwrap_or(""),
    ) {
        dhuhr.adhan = clock;
    }
    if let Some(clock) = parse_hhmm(
        settings
            .get("jumuah_iqamah")
            .map(String::as_str)
            .unwrap_or(""),
    ) {
        dhuhr.iqamah = Some(clock);
    }
}

struct Event {
    prayer: usize,
    at_unix_ms: u64,
    minutes: u16,
    iqamah: bool,
}

fn event_instants(prayers: &[Prayer], now: &Zoned, date: Date) -> Vec<Event> {
    let mut events = Vec::new();
    for (index, row) in prayers.iter().enumerate() {
        if let Some(at_unix_ms) = civil_clock_ms(now, date, row.adhan) {
            events.push(Event {
                prayer: index,
                at_unix_ms,
                minutes: minutes_of_day(row.adhan.hour, row.adhan.minute),
                iqamah: false,
            });
        }
        if let Some(iqamah) = row.iqamah {
            if let Some(at_unix_ms) = civil_clock_ms(now, date, iqamah) {
                events.push(Event {
                    prayer: index,
                    at_unix_ms,
                    minutes: minutes_of_day(iqamah.hour, iqamah.minute),
                    iqamah: true,
                });
            }
        }
    }
    events
}

fn silence_worthy(prayers: &[Prayer], event: &Event) -> bool {
    let Some(row) = prayers.get(event.prayer) else {
        return false;
    };
    if row.name == "Sunrise" {
        return false;
    }
    event.iqamah || row.iqamah.is_none()
}

fn phase_at(
    now_unix_ms: u64,
    current: u16,
    prayers: &[Prayer],
    events: &[Event],
    settings: &BTreeMap<String, String>,
    now: &Zoned,
) -> Option<(usize, bool, Phase, u64)> {
    let countdown_s = u32::try_from(int_in(settings, "countdown_s", 60, 0, 600)).ok()?;
    let silence_s = u32::try_from(int_in(settings, "silence_s", 0, 0, 3_600)).ok()?;
    let upcoming = events
        .iter()
        .find(|event| event.minutes > current)
        .or_else(|| events.first())?;
    let next = upcoming.prayer;
    let midnight = midnight_tomorrow_ms(now)?;
    if upcoming.minutes <= current {
        return Some((0, false, Phase::Table, midnight));
    }
    let remain_ms = upcoming.at_unix_ms.saturating_sub(now_unix_ms);
    let remain_s = u32::try_from(remain_ms / 1_000).unwrap_or(u32::MAX);
    if countdown_s > 0 && remain_s > 0 && remain_s <= countdown_s {
        let label = if upcoming.iqamah { "Iqamah" } else { "Adhan" };
        return Some((
            next,
            upcoming.iqamah,
            Phase::Countdown { label, remain_s },
            now_unix_ms.saturating_add(1_000),
        ));
    }
    if silence_s > 0 {
        if let Some(started) = events
            .iter()
            .rev()
            .find(|event| event.minutes <= current && silence_worthy(prayers, event))
        {
            let elapsed_s = now_unix_ms.saturating_sub(started.at_unix_ms) / 1_000;
            if elapsed_s < u64::from(silence_s) {
                let until = started
                    .at_unix_ms
                    .saturating_add(u64::from(silence_s) * 1_000);
                return Some((started.prayer, started.iqamah, Phase::Silence, until));
            }
        }
    }
    Some((next, upcoming.iqamah, Phase::Table, upcoming.at_unix_ms))
}

/// What to do when a twilight angle never arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HighLatitude {
    /// Leave it absent. Honest, and what the renderer did before — but it
    /// blanks the card, so it is not the default.
    None,
    /// Split the night into halves; fajr and isha sit at the boundary.
    MiddleOfNight,
    /// A seventh of the night after sunset, and before sunrise.
    SeventhOfNight,
    /// The twilight angle as a proportion of the night.
    AngleBased,
}

fn rule_of(settings: &BTreeMap<String, String>) -> HighLatitude {
    match settings
        .get("high_lat_rule")
        .map(String::as_str)
        .unwrap_or("")
        .trim()
    {
        "none" => HighLatitude::None,
        "seventh" => HighLatitude::SeventhOfNight,
        "angle" => HighLatitude::AngleBased,
        _ => HighLatitude::MiddleOfNight,
    }
}

/// Fill fajr and isha when the angle never happened.
///
/// The night is sunset to sunrise; each rule says how far into it the two
/// prayers fall. A rule cannot be applied without both ends of the night, so
/// a polar day leaves them absent — which is the one case where absent is the
/// only true answer rather than a missing feature.
fn high_latitude(mut hours: Hours, method: &Method, rule: HighLatitude) -> Hours {
    if rule == HighLatitude::None {
        return hours;
    }
    let (Some(sunrise), Some(sunset)) = (hours.sunrise, hours.maghrib) else {
        return hours;
    };
    let night = fixhour(sunrise - sunset);
    let portion = |angle: f64| match rule {
        HighLatitude::MiddleOfNight => night / 2.0,
        HighLatitude::SeventhOfNight => night / 7.0,
        HighLatitude::AngleBased => night / 60.0 * angle,
        HighLatitude::None => night,
    };
    if hours.fajr.is_none() {
        hours.fajr = Some(fixhour(sunrise - portion(method.fajr)));
    }
    if hours.isha.is_none() {
        let angle = method.isha_min.map_or(method.isha, |_| 18.0);
        hours.isha = Some(fixhour(sunset + portion(angle)));
    }
    hours
}

fn parse_method(raw: &str) -> Method {
    let key = raw.trim().to_ascii_lowercase();
    match key.as_str() {
        "isna" | "2" => ISNA,
        "egypt" | "egyptian" | "5" => EGYPT,
        "makkah" | "mecca" | "umm al-qura" | "4" => MAKKAH,
        "karachi" | "1" => KARACHI,
        "tehran" | "7" => TEHRAN,
        "jafari" | "shia" | "0" => JAFARI,
        "mwl" | "3" | "" => MWL,
        _ => MWL,
    }
}

struct Hours {
    fajr: Option<f64>,
    sunrise: Option<f64>,
    dhuhr: Option<f64>,
    asr: Option<f64>,
    maghrib: Option<f64>,
    isha: Option<f64>,
}

fn compute(lat: f64, lng: f64, tz: f64, date: Date, method: Method, asr_factor: f64) -> Hours {
    let jd = julian(
        i32::from(date.year()),
        i32::from(date.month()),
        i32::from(date.day()),
    ) - lng / (15.0 * 24.0);
    let mut fajr = None;
    let mut sunrise = None;
    let mut dhuhr = None;
    let mut asr = None;
    let mut sunset = None;
    let mut maghrib = None;
    let mut isha = None;
    for _ in 0..2 {
        fajr = sun_angle(jd, lat, method.fajr, fajr.unwrap_or(5.0) / 24.0, true);
        sunrise = sun_angle(jd, lat, 0.833, sunrise.unwrap_or(6.0) / 24.0, true);
        dhuhr = Some(midday(jd, dhuhr.unwrap_or(12.0) / 24.0));
        asr = asr_time(jd, lat, asr_factor, asr.unwrap_or(13.0) / 24.0);
        sunset = sun_angle(jd, lat, 0.833, sunset.unwrap_or(18.0) / 24.0, false);
        maghrib = match (sunset, method.maghrib_min) {
            (Some(set), Some(minutes)) => Some(set + minutes / 60.0),
            (Some(set), None) => Some(set),
            (None, _) => None,
        };
        isha = match method.isha_min {
            Some(minutes) => maghrib.map(|value| value + minutes / 60.0),
            None => sun_angle(jd, lat, method.isha, isha.unwrap_or(18.0) / 24.0, false),
        };
    }
    let shift = tz - lng / 15.0;
    let wrap = |value: Option<f64>| value.map(|hours| fixhour(hours + shift));
    Hours {
        fajr: wrap(fajr),
        sunrise: wrap(sunrise),
        dhuhr: wrap(dhuhr),
        asr: wrap(asr),
        maghrib: wrap(maghrib),
        isha: wrap(isha),
    }
}

fn hours_to_clock(hours: f64) -> Option<Clock> {
    if !hours.is_finite() {
        return None;
    }
    let wrapped = fixhour(hours);
    let hour = wrapped.floor();
    let minute = ((wrapped - hour) * 60.0).round();
    let mut h = hour as i32;
    let mut m = minute as i32;
    if m == 60 {
        m = 0;
        h += 1;
    }
    h = h.rem_euclid(24);
    Some(Clock {
        hour: u8::try_from(h).ok()?,
        minute: u8::try_from(m).ok()?,
    })
}

fn add_minutes(clock: Clock, minutes: i32) -> Option<Clock> {
    let total = i32::from(clock.hour) * 60 + i32::from(clock.minute) + minutes;
    let wrapped = total.rem_euclid(24 * 60);
    Some(Clock {
        hour: u8::try_from(wrapped / 60).ok()?,
        minute: u8::try_from(wrapped % 60).ok()?,
    })
}

fn parse_hhmm(raw: &str) -> Option<Clock> {
    let (h, m) = raw.split_once(':')?;
    let hour: u8 = h.parse().ok()?;
    let minute: u8 = m.parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(Clock { hour, minute })
}

pub fn format_clock(clock: Clock, clock_24h: bool) -> String {
    if clock_24h {
        format!("{:02}:{:02}", clock.hour, clock.minute)
    } else {
        let period = if clock.hour >= 12 { "PM" } else { "AM" };
        let hour12 = match clock.hour % 12 {
            0 => 12,
            n => n,
        };
        format!("{hour12}:{:02} {period}", clock.minute)
    }
}

fn flag(settings: &BTreeMap<String, String>, key: &str, default: bool) -> bool {
    match settings.get(key).map(String::as_str) {
        Some("0") | Some("false") => false,
        Some("1") | Some("true") => true,
        Some(_) => default,
        None => default,
    }
}

fn int_in(settings: &BTreeMap<String, String>, key: &str, default: i32, lo: i32, hi: i32) -> i32 {
    settings
        .get(key)
        .and_then(|raw| raw.parse().ok())
        .map(|value| clamp_i32(value, lo, hi))
        .unwrap_or(default)
}

fn clamp_i32(value: i32, lo: i32, hi: i32) -> i32 {
    value.clamp(lo, hi)
}

fn parse_theme(raw: &str) -> Theme {
    match raw.trim().to_ascii_lowercase().as_str() {
        "paper" => Theme::Paper,
        "emerald" => Theme::Emerald,
        "night" => Theme::Night,
        _ => Theme::Ink,
    }
}

fn hijri_label(date: Date, offset_days: i32) -> String {
    let shifted = date.checked_add(offset_days.days()).unwrap_or(date);
    let (year, month, day) = gregorian_to_hijri(
        i32::from(shifted.year()),
        i32::from(shifted.month()),
        i32::from(shifted.day()),
    );
    let names = [
        "Muharram",
        "Safar",
        "Rabi I",
        "Rabi II",
        "Jumada I",
        "Jumada II",
        "Rajab",
        "Shaban",
        "Ramadan",
        "Shawwal",
        "Dhul-Qadah",
        "Dhul-Hijjah",
    ];
    let month_name = names
        .get((month.saturating_sub(1) as usize).min(11))
        .copied()
        .unwrap_or("Muharram");
    format!("{day} {month_name} {year}")
}

fn gregorian_to_hijri(year: i32, month: i32, day: i32) -> (i32, i32, i32) {
    let jd = julian(year, month, day).floor() as i32;
    let l = jd - 1_948_440 + 10_632;
    let n = (l - 1) / 10_631;
    let l = l - 10_631 * n + 354;
    let j = ((10_985 - l) / 5316) * ((50 * l) / 17_719) + (l / 5670) * ((43 * l) / 15_238);
    let l = l - ((30 - j) / 15) * ((17_719 * j) / 50) - (j / 16) * ((15_238 * j) / 43) + 29;
    let m = (24 * l) / 709;
    let d = l - (709 * m) / 24;
    let y = 30 * n + j - 30;
    (y, m, d)
}

fn civil_clock_ms(now: &Zoned, date: Date, clock: Clock) -> Option<u64> {
    let hour = i8::try_from(clock.hour).ok()?;
    let minute = i8::try_from(clock.minute).ok()?;
    let zoned = date
        .at(hour, minute, 0, 0)
        .to_zoned(now.time_zone().clone())
        .ok()?;
    u64::try_from(zoned.timestamp().as_millisecond()).ok()
}

fn minutes_of_day(hour: u8, minute: u8) -> u16 {
    u16::from(hour)
        .saturating_mul(60)
        .saturating_add(u16::from(minute))
}

fn midnight_tomorrow_ms(now: &Zoned) -> Option<u64> {
    let tomorrow = now.date().checked_add(1.day()).ok()?;
    let zoned = tomorrow
        .at(0, 0, 0, 0)
        .to_zoned(now.time_zone().clone())
        .ok()?;
    u64::try_from(zoned.timestamp().as_millisecond()).ok()
}

fn julian(year: i32, month: i32, day: i32) -> f64 {
    let (mut year, mut month) = (year, month);
    if month <= 2 {
        year -= 1;
        month += 12;
    }
    let century = (f64::from(year) / 100.0).floor();
    let b = 2.0 - century + (century / 4.0).floor();
    (365.25 * (f64::from(year) + 4716.0)).floor()
        + (30.6001 * (f64::from(month) + 1.0)).floor()
        + f64::from(day)
        + b
        - 1524.5
}

fn sin(deg: f64) -> f64 {
    deg.to_radians().sin()
}
fn cos(deg: f64) -> f64 {
    deg.to_radians().cos()
}
fn tan(deg: f64) -> f64 {
    deg.to_radians().tan()
}
fn arcsin(x: f64) -> f64 {
    x.asin().to_degrees()
}
fn arccos(x: f64) -> f64 {
    x.acos().to_degrees()
}
fn arctan2(y: f64, x: f64) -> f64 {
    y.atan2(x).to_degrees()
}
fn arccot(x: f64) -> f64 {
    (1.0 / x).atan().to_degrees()
}

fn fix(value: f64, modulus: f64) -> f64 {
    let mut wrapped = value % modulus;
    if wrapped < 0.0 {
        wrapped += modulus;
    }
    wrapped
}

fn fixangle(deg: f64) -> f64 {
    fix(deg, 360.0)
}

fn fixhour(hours: f64) -> f64 {
    fix(hours, 24.0)
}

struct Sun {
    declination: f64,
    equation: f64,
}

fn sun_position(jd: f64) -> Sun {
    let day = jd - 2_451_545.0;
    let g = fixangle(357.529 + 0.985_600_28 * day);
    let q = fixangle(280.459 + 0.985_647_36 * day);
    let l = fixangle(q + 1.915 * sin(g) + 0.020 * sin(2.0 * g));
    let e = 23.439 - 0.000_000_36 * day;
    let ra = fixhour(arctan2(cos(e) * sin(l), cos(l)) / 15.0);
    Sun {
        declination: arcsin(sin(e) * sin(l)),
        equation: q / 15.0 - ra,
    }
}

fn midday(jd: f64, portion: f64) -> f64 {
    let eqt = sun_position(jd + portion).equation;
    fixhour(12.0 - eqt)
}

fn sun_angle(jd: f64, lat: f64, angle: f64, portion: f64, before: bool) -> Option<f64> {
    let decl = sun_position(jd + portion).declination;
    let noon = midday(jd, portion);
    let numerator = -sin(angle) - sin(decl) * sin(lat);
    let denominator = cos(decl) * cos(lat);
    if denominator.abs() < f64::EPSILON {
        return None;
    }
    let cosine = numerator / denominator;
    if !(-1.0..=1.0).contains(&cosine) {
        return None;
    }
    let t = arccos(cosine) / 15.0;
    Some(if before { noon - t } else { noon + t })
}

fn asr_time(jd: f64, lat: f64, factor: f64, portion: f64) -> Option<f64> {
    let decl = sun_position(jd + portion).declination;
    let angle = -arccot(factor + tan((lat - decl).abs()));
    sun_angle(jd, lat, angle, portion, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn london_new_year() -> DayTimes {
        let mut settings = BTreeMap::new();
        settings.insert("latitude".into(), "51.5074".into());
        settings.insert("longitude".into(), "-0.1278".into());
        settings.insert("method".into(), "isna".into());
        settings.insert("timezone".into(), "Europe/London".into());
        // 2024-01-01 12:00 UTC
        times_from_settings(&settings, 1_704_110_400_000).expect("london times")
    }

    #[test]
    fn isna_london_in_january_has_a_short_day() {
        let day = london_new_year();
        assert_eq!(day.prayers[0].name, "Fajr");
        assert_eq!(day.prayers[5].name, "Isha");
        let fajr = minutes_of_day(day.prayers[0].adhan.hour, day.prayers[0].adhan.minute);
        let sunrise = minutes_of_day(day.prayers[1].adhan.hour, day.prayers[1].adhan.minute);
        let maghrib = minutes_of_day(day.prayers[4].adhan.hour, day.prayers[4].adhan.minute);
        let isha = minutes_of_day(day.prayers[5].adhan.hour, day.prayers[5].adhan.minute);
        assert!(fajr < sunrise, "fajr precedes sunrise");
        assert!(sunrise < minutes_of_day(day.prayers[2].adhan.hour, day.prayers[2].adhan.minute));
        assert!(maghrib < isha);
        // London in January: sunrise after 07:30, maghrib before 17:00 GMT.
        assert!(sunrise >= 7 * 60 + 30);
        assert!(maghrib <= 17 * 60);
    }

    #[test]
    fn missing_coordinates_are_not_a_timetable() {
        let settings = BTreeMap::from([("method".into(), "isna".into())]);
        assert!(times_from_settings(&settings, 1_704_110_400_000).is_none());
    }

    #[test]
    fn dhuhr_iqamah_is_after_adhan() {
        let mut settings = BTreeMap::new();
        settings.insert("latitude".into(), "51.5074".into());
        settings.insert("longitude".into(), "-0.1278".into());
        settings.insert("method".into(), "isna".into());
        settings.insert("timezone".into(), "Europe/London".into());
        settings.insert("iqamah_dhuhr".into(), "15".into());
        let day = times_from_settings(&settings, 1_704_110_400_000).expect("times");
        let dhuhr = day
            .prayers
            .iter()
            .find(|row| row.name == "Dhuhr")
            .expect("dhuhr");
        let iqamah = dhuhr.iqamah.expect("offset");
        let adhan_m = minutes_of_day(dhuhr.adhan.hour, dhuhr.adhan.minute);
        let iqamah_m = minutes_of_day(iqamah.hour, iqamah.minute);
        assert_eq!(iqamah_m, adhan_m + 15);
        assert!(day.show_iqamah);
    }

    #[test]
    fn space_config_overlays_the_snapshot() {
        let entry = BTreeMap::from([("latitude".into(), "0".into())]);
        let space = BTreeMap::from([
            ("latitude".into(), "51.5".into()),
            ("theme".into(), "emerald".into()),
        ]);
        let merged = overlay(Some(&space), &entry);
        assert_eq!(merged.get("latitude").map(String::as_str), Some("51.5"));
        assert_eq!(merged.get("theme").map(String::as_str), Some("emerald"));
    }

    fn london(extra: &[(&str, &str)], now_unix_ms: u64) -> DayTimes {
        let mut settings = BTreeMap::from([
            ("latitude".into(), "51.5074".into()),
            ("longitude".into(), "-0.1278".into()),
            ("method".into(), "isna".into()),
            ("timezone".into(), "Europe/London".into()),
        ]);
        for (key, value) in extra {
            settings.insert((*key).into(), (*value).into());
        }
        times_from_settings(&settings, now_unix_ms).expect("london times")
    }

    fn prayer<'a>(day: &'a DayTimes, name: &str) -> &'a Prayer {
        day.prayers
            .iter()
            .find(|row| row.name == name)
            .unwrap_or_else(|| panic!("{name} row"))
    }

    fn civil_ms(year: i16, month: i8, day: i8, hour: u8, minute: u8) -> u64 {
        let zone = TimeZone::get("Europe/London").unwrap();
        let date = Date::new(year, month, day).unwrap();
        let hour = i8::try_from(hour).unwrap();
        let minute = i8::try_from(minute).unwrap();
        u64::try_from(
            date.at(hour, minute, 0, 0)
                .to_zoned(zone)
                .unwrap()
                .timestamp()
                .as_millisecond(),
        )
        .unwrap()
    }

    #[test]
    fn hanafi_asr_is_later_than_shafi() {
        let now = 1_704_110_400_000;
        let shafi_day = london(&[], now);
        let shafi = minutes_of_day(
            prayer(&shafi_day, "Asr").adhan.hour,
            prayer(&shafi_day, "Asr").adhan.minute,
        );
        let hanafi = london(&[("asr_school", "hanafi")], now);
        let hanafi_m = minutes_of_day(
            prayer(&hanafi, "Asr").adhan.hour,
            prayer(&hanafi, "Asr").adhan.minute,
        );
        assert!(hanafi_m > shafi, "hanafi {hanafi_m} shafi {shafi}");
    }

    #[test]
    fn jumuah_clocks_rewrite_friday_dhuhr_only() {
        // 2024-01-05 is Friday; 2024-01-04 is Thursday. Noon UTC both days.
        let friday = 1_704_456_000_000;
        let thursday = 1_704_369_600_000;
        let keys = [("jumuah_khutbah", "13:15"), ("jumuah_iqamah", "13:45")];
        let friday_day = london(&keys, friday);
        let friday_dhuhr = prayer(&friday_day, "Dhuhr");
        assert_eq!(
            friday_dhuhr.adhan,
            Clock {
                hour: 13,
                minute: 15
            }
        );
        assert_eq!(
            friday_dhuhr.iqamah,
            Some(Clock {
                hour: 13,
                minute: 45
            })
        );
        let thursday_day = london(&keys, thursday);
        let thursday_dhuhr = prayer(&thursday_day, "Dhuhr");
        assert_ne!(
            thursday_dhuhr.adhan,
            Clock {
                hour: 13,
                minute: 15
            }
        );
    }

    #[test]
    fn countdown_starts_inside_the_window() {
        let noon = london(&[], 1_704_110_400_000);
        let dhuhr = prayer(&noon, "Dhuhr");
        let at = civil_ms(2024, 1, 1, dhuhr.adhan.hour, dhuhr.adhan.minute);
        let day = london(&[("countdown_s", "60")], at.saturating_sub(30_000));
        assert!(
            matches!(
                day.phase,
                Phase::Countdown {
                    label: "Adhan",
                    remain_s: 29..=31
                }
            ),
            "{:?}",
            day.phase
        );
        assert!(day.next_change_unix_ms.abs_diff(at.saturating_sub(29_000)) <= 1_000);
    }

    #[test]
    fn silence_follows_iqamah_and_not_sunrise() {
        let noon = london(&[("iqamah_dhuhr", "15")], 1_704_110_400_000);
        let dhuhr = prayer(&noon, "Dhuhr");
        let iqamah = dhuhr.iqamah.expect("offset");
        let after_iqamah = civil_ms(2024, 1, 1, iqamah.hour, iqamah.minute).saturating_add(30_000);
        let silent = london(
            &[
                ("iqamah_dhuhr", "15"),
                ("silence_s", "120"),
                ("countdown_s", "0"),
            ],
            after_iqamah,
        );
        assert_eq!(silent.phase, Phase::Silence);

        let sunrise = prayer(&noon, "Sunrise");
        let after_sunrise =
            civil_ms(2024, 1, 1, sunrise.adhan.hour, sunrise.adhan.minute).saturating_add(30_000);
        let not_sunrise = london(
            &[("silence_s", "3600"), ("countdown_s", "0")],
            after_sunrise,
        );
        assert_eq!(not_sunrise.phase, Phase::Table);
    }
}
