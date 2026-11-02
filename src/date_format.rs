//! Locale-aware date and time text.
//!
//! Translators own the vocabulary (month and weekday names, AM/PM markers)
//! and CLDR-style patterns such as `MMM d, y`, so each language decides the
//! order, punctuation and compactness of its dates instead of only naming the
//! months. Controllers format through the formats loaded for the UI thread.
//! They are reloaded whenever the language or the time format changes, and
//! views registered with [`on_change`] then rebuild their date text.

use crate::{AppWindow, I18n};
use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
use flectar_mail_core::models::TimeFormat;
use slint::ComponentHandle;
use std::{cell::RefCell, fmt::Write, rc::Rc, time::Duration};

/// The clock used for times of day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HourCycle {
    Twelve,
    TwentyFour,
}

impl HourCycle {
    pub(crate) fn for_preference(preference: TimeFormat) -> Self {
        match preference {
            TimeFormat::Automatic => regional_hour_cycle(time_locale().as_deref()),
            TimeFormat::TwelveHour => Self::Twelve,
            TimeFormat::TwentyFourHour => Self::TwentyFour,
        }
    }
}

/// How much of a date to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DateStyle {
    /// Day and month in dense lists, such as "Oct 22".
    ShortDayMonth,
    /// A date with its year in dense lists, such as "Oct 22, 2026".
    Short,
    /// A date on its own, such as "Oct 22, 2026".
    Medium,
    /// A date with its weekday, such as "Thu, Oct 22, 2026".
    WeekdayMedium,
    /// A day heading without the year, such as "Thursday, October 22".
    WeekdayDayMonth,
    /// A month heading, such as "October 2026".
    MonthYear,
}

impl DateStyle {
    fn pattern(self) -> PatternKey {
        match self {
            Self::ShortDayMonth => PatternKey::ShortDayMonth,
            Self::Short => PatternKey::ShortDate,
            Self::Medium => PatternKey::Date,
            Self::WeekdayMedium => PatternKey::WeekdayDate,
            Self::WeekdayDayMonth => PatternKey::WeekdayDayMonth,
            Self::MonthYear => PatternKey::MonthYear,
        }
    }
}

/// Translated vocabulary and parsed patterns for one language and clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DateFormats {
    hour_cycle: HourCycle,
    months: [String; 12],
    abbreviated_months: [String; 12],
    standalone_months: [String; 12],
    /// Monday first, as `Weekday::num_days_from_monday` counts.
    weekdays: [String; 7],
    abbreviated_weekdays: [String; 7],
    am: String,
    pm: String,
    yesterday: String,
    patterns: Vec<Pattern>,
    range: Join,
    date_at_time: Join,
}

impl DateFormats {
    /// The source-language formats, used until the catalog is loaded.
    pub(crate) fn english(hour_cycle: HourCycle) -> Self {
        Self::from_vocabulary(&English, hour_cycle)
    }

    fn load(i18n: &I18n<'_>, hour_cycle: HourCycle) -> Self {
        Self::from_vocabulary(&Catalog(i18n), hour_cycle)
    }

    fn from_vocabulary(vocabulary: &impl Vocabulary, hour_cycle: HourCycle) -> Self {
        let months = |form: MonthForm| -> [String; 12] {
            std::array::from_fn(|index| vocabulary.month(form, index as u32 + 1))
        };
        let weekdays = |abbreviated: bool| -> [String; 7] {
            std::array::from_fn(|index| vocabulary.weekday(abbreviated, index as u32))
        };
        Self {
            hour_cycle,
            months: months(MonthForm::Wide),
            abbreviated_months: months(MonthForm::Abbreviated),
            standalone_months: months(MonthForm::Standalone),
            weekdays: weekdays(false),
            abbreviated_weekdays: weekdays(true),
            am: vocabulary.day_period(false),
            pm: vocabulary.day_period(true),
            yesterday: vocabulary.yesterday(),
            // A pattern the translation broke falls back to the source
            // pattern rather than printing the mistake into every date.
            patterns: PatternKey::ALL
                .iter()
                .map(|&key| {
                    Pattern::parse(&vocabulary.pattern(key))
                        .or_else(|_| Pattern::parse(key.english()))
                        .expect("source date patterns parse")
                })
                .collect(),
            range: Join::parse(&vocabulary.join(JoinKey::Range))
                .or_else(|| Join::parse(JoinKey::Range.english()))
                .expect("source date range template parses"),
            date_at_time: Join::parse(&vocabulary.join(JoinKey::DateAtTime))
                .or_else(|| Join::parse(JoinKey::DateAtTime.english()))
                .expect("source date and time template parses"),
        }
    }

    pub(crate) fn date(&self, style: DateStyle, date: NaiveDate) -> String {
        self.format(style.pattern(), date, 0, 0)
    }

    /// A time of day; `hour` may be 24 for the end of a day.
    pub(crate) fn time(&self, hour: u32, minute: u32) -> String {
        let pattern = match self.hour_cycle {
            HourCycle::Twelve => PatternKey::Time12,
            HourCycle::TwentyFour => PatternKey::Time24,
        };
        self.format(pattern, NaiveDate::MIN, hour, minute)
    }

    /// Reads a time typed on either clock: "14:30", "14.30", "2:30 PM", or
    /// this language's day period such as "午後0:30". Returns the hour and
    /// minute.
    pub(crate) fn parse_time(&self, text: &str) -> Option<(u32, u32)> {
        let compact = |text: &str| {
            text.chars()
                .filter(|ch| !ch.is_whitespace())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        };
        let text = compact(text);
        let mut periods = [
            (compact(&self.am), false),
            (compact(&self.pm), true),
            ("a.m.".to_owned(), false),
            ("p.m.".to_owned(), true),
            ("am".to_owned(), false),
            ("pm".to_owned(), true),
        ];
        // Longer markers first, so "a.m." is not read as "am" with stray dots.
        periods.sort_by_key(|(marker, _)| std::cmp::Reverse(marker.len()));
        let (clock, pm) = periods
            .iter()
            .filter(|(marker, _)| !marker.is_empty())
            .find_map(|(marker, pm)| {
                let at = text.find(marker.as_str())?;
                let clock = format!("{}{}", &text[..at], &text[at + marker.len()..]);
                Some((clock, Some(*pm)))
            })
            .unwrap_or((text, None));
        let clock = clock.trim_matches('.');
        let separated = clock.split_once([':', '.']);
        let (hour, minute) = separated.unwrap_or((clock, "0"));
        let all_digits = |part: &str| {
            (1..=2).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_digit())
        };
        if !all_digits(hour) || !all_digits(minute) || (pm.is_none() && separated.is_none()) {
            return None;
        }
        let (hour, minute) = (hour.parse::<u32>().ok()?, minute.parse::<u32>().ok()?);
        // A 12-hour clock may count from 0 ("午後0:05") or from 12 ("12:05 PM").
        let hour = match pm {
            Some(pm) if hour <= 12 => hour % 12 + if pm { 12 } else { 0 },
            Some(_) => return None,
            None => hour,
        };
        (hour < 24 && minute < 60).then_some((hour, minute))
    }

    /// A whole hour, as calendar time rulers label them.
    pub(crate) fn hour(&self, hour: u32) -> String {
        match self.hour_cycle {
            HourCycle::Twelve => self.format(PatternKey::Hour12, NaiveDate::MIN, hour, 0),
            HourCycle::TwentyFour => self.time(hour, 0),
        }
    }

    pub(crate) fn date_time(&self, style: DateStyle, value: NaiveDateTime) -> String {
        self.date_at_time.apply(
            &self.date(style, value.date()),
            &self.time(value.hour(), value.minute()),
        )
    }

    pub(crate) fn range(&self, start: &str, end: &str) -> String {
        self.range.apply(start, end)
    }

    /// Two dates of one span, leaving out the month and year they share.
    pub(crate) fn date_range(&self, start: NaiveDate, end: NaiveDate) -> String {
        let (start_pattern, end_pattern) = if start.year() != end.year() {
            (PatternKey::Date, PatternKey::Date)
        } else if start.month() != end.month() {
            (PatternKey::SameYearStart, PatternKey::SameYearEnd)
        } else {
            (PatternKey::SameMonthStart, PatternKey::SameMonthEnd)
        };
        self.range(
            &self.format(start_pattern, start, 0, 0),
            &self.format(end_pattern, end, 0, 0),
        )
    }

    pub(crate) fn yesterday(&self) -> &str {
        &self.yesterday
    }

    /// A weekday name, counting from Monday.
    pub(crate) fn weekday(&self, days_from_monday: u32, abbreviated: bool) -> &str {
        let names = if abbreviated {
            &self.abbreviated_weekdays
        } else {
            &self.weekdays
        };
        &names[days_from_monday as usize % 7]
    }

    fn format(&self, key: PatternKey, date: NaiveDate, hour: u32, minute: u32) -> String {
        let mut text = String::new();
        for field in &self.patterns[key as usize].0 {
            match *field {
                Field::Literal(ref literal) => text.push_str(literal),
                Field::Year(2) => push_number(&mut text, date.year().rem_euclid(100), 2),
                Field::Year(width) => push_number(&mut text, date.year(), width),
                Field::Month(width) | Field::StandaloneMonth(width) if width <= 2 => {
                    push_number(&mut text, date.month() as i32, width)
                }
                Field::Month(3) | Field::StandaloneMonth(3) => {
                    text.push_str(&self.abbreviated_months[date.month0() as usize])
                }
                Field::Month(_) => text.push_str(&self.months[date.month0() as usize]),
                Field::StandaloneMonth(_) => {
                    text.push_str(&self.standalone_months[date.month0() as usize])
                }
                Field::Day(width) => push_number(&mut text, date.day() as i32, width),
                Field::Weekday(width) => {
                    text.push_str(self.weekday(date.weekday().num_days_from_monday(), width <= 3))
                }
                Field::DayPeriod => text.push_str(if hour % 24 < 12 { &self.am } else { &self.pm }),
                Field::Hour(clock, width) => {
                    let value = match clock {
                        Clock::OneToTwelve if hour.is_multiple_of(12) => 12,
                        Clock::OneToTwelve | Clock::ZeroToEleven => hour % 12,
                        Clock::ZeroToTwentyThree => hour,
                        Clock::OneToTwentyFour if hour == 0 => 24,
                        Clock::OneToTwentyFour => hour,
                    };
                    push_number(&mut text, value as i32, width)
                }
                Field::Minute(width) => push_number(&mut text, minute as i32, width),
            }
        }
        text
    }
}

/// Writes `value` zero-padded to `width` digits.
fn push_number(text: &mut String, value: i32, width: usize) {
    let _ = write!(text, "{value:0width$}");
}

thread_local! {
    // The UI thread formats every date, so each thread keeps its own formats
    // and tests running in parallel threads stay independent.
    static CURRENT: RefCell<Rc<DateFormats>> =
        RefCell::new(Rc::new(DateFormats::english(HourCycle::TwentyFour)));
    static LISTENERS: RefCell<Vec<Rc<dyn Fn()>>> = const { RefCell::new(Vec::new()) };
}

/// The formats for the current language and time format.
pub(crate) fn current() -> Rc<DateFormats> {
    CURRENT.with(|current| Rc::clone(&current.borrow()))
}

/// Loads the formats after the language or the time format changed. Views
/// registered with [`on_change`] rebuild their text on the next event-loop
/// turn, outside whatever state the caller is holding.
pub(crate) fn reload(app: &AppWindow) {
    let preference = TimeFormat::parse(&app.get_time_format()).unwrap_or_default();
    let formats = DateFormats::load(&app.global::<I18n>(), HourCycle::for_preference(preference));
    app.set_time_format_sample(formats.time(13, 30).into());
    let changed = CURRENT.with(|current| {
        let mut current = current.borrow_mut();
        let changed = **current != formats;
        if changed {
            *current = Rc::new(formats);
        }
        changed
    });
    let listeners = LISTENERS.with(|listeners| listeners.borrow().clone());
    if changed && !listeners.is_empty() {
        slint::Timer::single_shot(Duration::ZERO, move || {
            for listener in &listeners {
                listener();
            }
        });
    }
}

/// Runs `listener` after each change of the formats.
pub(crate) fn on_change(listener: impl Fn() + 'static) {
    LISTENERS.with(|listeners| listeners.borrow_mut().push(Rc::new(listener)));
}

/// Regions whose preferred clock is 12-hour, from CLDR 48 `timeData`.
const TWELVE_HOUR_REGIONS: [&str; 105] = [
    "419", "AE", "AG", "AL", "AR", "AS", "AU", "BB", "BD", "BH", "BM", "BN", "BO", "BS", "BT",
    "CA", "CL", "CO", "CR", "CU", "CY", "DJ", "DM", "DO", "DZ", "EC", "EG", "EH", "ER", "ET", "FJ",
    "FM", "GD", "GH", "GM", "GR", "GT", "GU", "GY", "HK", "HN", "IN", "IQ", "JM", "JO", "KH", "KI",
    "KN", "KP", "KR", "KW", "KY", "LB", "LC", "LR", "LS", "LY", "MH", "MO", "MP", "MR", "MW", "MX",
    "MY", "NA", "NI", "NZ", "OM", "PA", "PE", "PG", "PH", "PK", "PR", "PS", "PW", "PY", "QA", "SA",
    "SB", "SD", "SG", "SL", "SO", "SS", "SV", "SY", "SZ", "TC", "TD", "TN", "TO", "TT", "TW", "UM",
    "US", "UY", "VC", "VE", "VG", "VI", "VU", "WS", "YE", "ZM",
];

/// Language and region pairs that differ from their region's clock.
const TWELVE_HOUR_LOCALES: [(&str, &str); 2] = [("ar", "001"), ("en", "001")];
const TWENTY_FOUR_HOUR_LOCALES: [(&str, &str); 2] = [("fr", "CA"), ("ku", "SY")];

/// Languages whose likely region prefers a 12-hour clock, for locales that
/// name no region (CLDR 48 `likelySubtags`).
const TWELVE_HOUR_LANGUAGES: [&str; 51] = [
    "aa", "ak", "am", "ar", "as", "ay", "bi", "bn", "ch", "cr", "dz", "ee", "el", "en", "fj", "gn",
    "gu", "hi", "ho", "hz", "ik", "iu", "kj", "km", "kn", "ko", "ks", "mh", "mi", "ml", "mr", "ms",
    "ng", "nv", "ny", "oj", "om", "or", "pa", "qu", "sa", "sd", "sm", "so", "sq", "ta", "te", "ti",
    "tl", "to", "ur",
];

/// The clock a locale such as `en_US.UTF-8` or `zh-Hans-TW` prefers.
fn regional_hour_cycle(locale: Option<&str>) -> HourCycle {
    let tag = locale
        .and_then(|locale| locale.split(['.', '@']).next())
        .unwrap_or_default();
    let mut subtags = tag.split(['-', '_']).filter(|subtag| !subtag.is_empty());
    let language = subtags.next().unwrap_or_default().to_ascii_lowercase();
    let region = subtags
        .find(|subtag| {
            (subtag.len() == 2 && subtag.bytes().all(|byte| byte.is_ascii_alphabetic()))
                || (subtag.len() == 3 && subtag.bytes().all(|byte| byte.is_ascii_digit()))
        })
        .map(str::to_ascii_uppercase);
    let twelve = match region.as_deref() {
        Some(region) => {
            let locale = (language.as_str(), region);
            if TWELVE_HOUR_LOCALES.contains(&locale) {
                true
            } else if TWENTY_FOUR_HOUR_LOCALES.contains(&locale) {
                false
            } else {
                TWELVE_HOUR_REGIONS.binary_search(&region).is_ok()
            }
        }
        None => TWELVE_HOUR_LANGUAGES
            .binary_search(&language.as_str())
            .is_ok(),
    };
    if twelve {
        HourCycle::Twelve
    } else {
        HourCycle::TwentyFour
    }
}

/// The locale whose conventions govern times. POSIX systems can set the time
/// locale apart from the display language.
fn time_locale() -> Option<String> {
    #[cfg(all(desktop, unix, not(target_os = "macos")))]
    {
        for name in ["LC_ALL", "LC_TIME", "LANG"] {
            if let Ok(value) = std::env::var(name)
                && !value.is_empty()
            {
                return Some(value);
            }
        }
    }
    sys_locale::get_locale()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MonthForm {
    /// Abbreviated, as used inside a date.
    Abbreviated,
    /// Full, as used inside a date; some languages inflect it there.
    Wide,
    /// Full, on its own as in a month heading.
    Standalone,
}

impl MonthForm {
    fn key(self) -> &'static str {
        match self {
            Self::Abbreviated => "abbreviated",
            Self::Wide => "wide",
            Self::Standalone => "standalone",
        }
    }
}

/// The patterns translators provide; the discriminant indexes
/// `DateFormats::patterns`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PatternKey {
    ShortDayMonth,
    ShortDate,
    Date,
    WeekdayDate,
    WeekdayDayMonth,
    MonthYear,
    SameMonthStart,
    SameMonthEnd,
    SameYearStart,
    SameYearEnd,
    Time24,
    Time12,
    Hour12,
}

impl PatternKey {
    const ALL: [Self; 13] = [
        Self::ShortDayMonth,
        Self::ShortDate,
        Self::Date,
        Self::WeekdayDate,
        Self::WeekdayDayMonth,
        Self::MonthYear,
        Self::SameMonthStart,
        Self::SameMonthEnd,
        Self::SameYearStart,
        Self::SameYearEnd,
        Self::Time24,
        Self::Time12,
        Self::Hour12,
    ];

    /// The key `I18n.date-pattern` looks up.
    fn key(self) -> &'static str {
        match self {
            Self::ShortDayMonth => "short-day-month",
            Self::ShortDate => "short-date",
            Self::Date => "date",
            Self::WeekdayDate => "weekday-date",
            Self::WeekdayDayMonth => "weekday-day-month",
            Self::MonthYear => "month-year",
            Self::SameMonthStart => "same-month-start",
            Self::SameMonthEnd => "same-month-end",
            Self::SameYearStart => "same-year-start",
            Self::SameYearEnd => "same-year-end",
            Self::Time24 => "time-24",
            Self::Time12 => "time-12",
            Self::Hour12 => "hour-12",
        }
    }

    /// The source pattern, matching the message id in `ui/app.slint`.
    fn english(self) -> &'static str {
        match self {
            Self::ShortDayMonth => "MMM d",
            Self::ShortDate => "MMM d, y",
            Self::Date => "MMM d, y",
            Self::WeekdayDate => "EEE, MMM d, y",
            Self::WeekdayDayMonth => "EEEE, MMMM d",
            Self::MonthYear => "LLLL y",
            Self::SameMonthStart => "MMMM d",
            Self::SameMonthEnd => "d, y",
            Self::SameYearStart => "MMM d",
            Self::SameYearEnd => "MMM d, y",
            Self::Time24 => "HH:mm",
            Self::Time12 => "h:mm a",
            Self::Hour12 => "h a",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JoinKey {
    Range,
    DateAtTime,
}

impl JoinKey {
    fn english(self) -> &'static str {
        match self {
            Self::Range => "{0} – {1}",
            Self::DateAtTime => "{0} at {1}",
        }
    }
}

/// Where the formats get their wording.
trait Vocabulary {
    fn month(&self, form: MonthForm, month: u32) -> String;
    fn weekday(&self, abbreviated: bool, days_from_monday: u32) -> String;
    fn day_period(&self, pm: bool) -> String;
    fn yesterday(&self) -> String;
    fn pattern(&self, key: PatternKey) -> String;
    /// The template with `{0}` and `{1}` marking its two parts.
    fn join(&self, key: JoinKey) -> String;
}

struct Catalog<'a>(&'a I18n<'a>);

impl Vocabulary for Catalog<'_> {
    fn month(&self, form: MonthForm, month: u32) -> String {
        self.0
            .invoke_date_month(month as i32, form.key().into())
            .into()
    }

    fn weekday(&self, abbreviated: bool, days_from_monday: u32) -> String {
        self.0
            .invoke_date_weekday(days_from_monday as i32, abbreviated)
            .into()
    }

    fn day_period(&self, pm: bool) -> String {
        self.0.invoke_date_day_period(pm).into()
    }

    fn yesterday(&self) -> String {
        self.0.invoke_date_yesterday().into()
    }

    fn pattern(&self, key: PatternKey) -> String {
        self.0.invoke_date_pattern(key.key().into()).into()
    }

    fn join(&self, key: JoinKey) -> String {
        let (first, second) = ("{0}".into(), "{1}".into());
        match key {
            JoinKey::Range => self.0.invoke_date_range(first, second),
            JoinKey::DateAtTime => self.0.invoke_date_at_time(first, second),
        }
        .into()
    }
}

struct English;

impl Vocabulary for English {
    fn month(&self, form: MonthForm, month: u32) -> String {
        const MONTHS: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        let name = MONTHS[(month as usize - 1) % 12];
        match form {
            MonthForm::Abbreviated => name[..3].to_owned(),
            MonthForm::Wide | MonthForm::Standalone => name.to_owned(),
        }
    }

    fn weekday(&self, abbreviated: bool, days_from_monday: u32) -> String {
        const WEEKDAYS: [&str; 7] = [
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
            "Sunday",
        ];
        let name = WEEKDAYS[days_from_monday as usize % 7];
        if abbreviated {
            name[..3].to_owned()
        } else {
            name.to_owned()
        }
    }

    fn day_period(&self, pm: bool) -> String {
        if pm { "PM" } else { "AM" }.to_owned()
    }

    fn yesterday(&self) -> String {
        "Yesterday".to_owned()
    }

    fn pattern(&self, key: PatternKey) -> String {
        key.english().to_owned()
    }

    fn join(&self, key: JoinKey) -> String {
        key.english().to_owned()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Clock {
    /// `h`: 1–12.
    OneToTwelve,
    /// `K`: 0–11.
    ZeroToEleven,
    /// `H`: 0–23, and 24 for the end of a day.
    ZeroToTwentyThree,
    /// `k`: 1–24.
    OneToTwentyFour,
}

/// A CLDR date field, carrying its symbol count.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Field {
    Literal(String),
    Year(usize),
    /// `M`: numeric up to two symbols, then abbreviated and full names.
    Month(usize),
    /// `L`: as `M`, with the full name in its standalone form.
    StandaloneMonth(usize),
    Day(usize),
    /// `E` or `c`: abbreviated up to three symbols, then the full name.
    Weekday(usize),
    DayPeriod,
    Hour(Clock, usize),
    Minute(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pattern(Vec<Field>);

impl Pattern {
    /// Parses CLDR pattern syntax. Text in single quotes is literal, `''` is
    /// an apostrophe, and other ASCII letters are reserved, so a symbol the
    /// formats don't support is an error rather than silently literal.
    fn parse(source: &str) -> Result<Self, char> {
        let mut fields = Vec::new();
        let mut literal = String::new();
        let mut chars = source.chars().peekable();
        while let Some(symbol) = chars.next() {
            if symbol == '\'' {
                if chars.next_if_eq(&'\'').is_some() {
                    literal.push('\'');
                    continue;
                }
                loop {
                    match chars.next() {
                        Some('\'') if chars.next_if_eq(&'\'').is_some() => literal.push('\''),
                        Some('\'') | None => break,
                        Some(quoted) => literal.push(quoted),
                    }
                }
                continue;
            }
            if !symbol.is_ascii_alphabetic() {
                literal.push(symbol);
                continue;
            }
            let mut width = 1;
            while chars.next_if_eq(&symbol).is_some() {
                width += 1;
            }
            let field = match symbol {
                'y' => Field::Year(width),
                'M' => Field::Month(width),
                'L' => Field::StandaloneMonth(width),
                'd' if width <= 2 => Field::Day(width),
                'E' => Field::Weekday(width),
                'c' if width >= 3 => Field::Weekday(width),
                'a' => Field::DayPeriod,
                'h' if width <= 2 => Field::Hour(Clock::OneToTwelve, width),
                'K' if width <= 2 => Field::Hour(Clock::ZeroToEleven, width),
                'H' if width <= 2 => Field::Hour(Clock::ZeroToTwentyThree, width),
                'k' if width <= 2 => Field::Hour(Clock::OneToTwentyFour, width),
                'm' if width <= 2 => Field::Minute(width),
                _ => return Err(symbol),
            };
            if !literal.is_empty() {
                fields.push(Field::Literal(std::mem::take(&mut literal)));
            }
            fields.push(field);
        }
        if !literal.is_empty() {
            fields.push(Field::Literal(literal));
        }
        Ok(Self(fields))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum JoinPart {
    Text(String),
    First,
    Second,
}

/// A translated template joining two formatted parts in either order.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Join(Vec<JoinPart>);

impl Join {
    /// Accepts a template with exactly one `{0}` and one `{1}`.
    fn parse(template: &str) -> Option<Self> {
        if template.matches("{0}").count() != 1 || template.matches("{1}").count() != 1 {
            return None;
        }
        let mut parts = Vec::new();
        let mut rest = template;
        while let Some(index) = rest.find("{0}").into_iter().chain(rest.find("{1}")).min() {
            if index > 0 {
                parts.push(JoinPart::Text(rest[..index].to_owned()));
            }
            parts.push(if rest[index..].starts_with("{0}") {
                JoinPart::First
            } else {
                JoinPart::Second
            });
            rest = &rest[index + 3..];
        }
        if !rest.is_empty() {
            parts.push(JoinPart::Text(rest.to_owned()));
        }
        Some(Self(parts))
    }

    fn apply(&self, first: &str, second: &str) -> String {
        self.0
            .iter()
            .map(|part| match part {
                JoinPart::Text(text) => text.as_str(),
                JoinPart::First => first,
                JoinPart::Second => second,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    /// Vocabulary a translation could supply; anything unset stays English.
    #[derive(Default)]
    struct Translation {
        patterns: &'static [(PatternKey, &'static str)],
        months: Option<[&'static str; 12]>,
        day_periods: Option<(&'static str, &'static str)>,
        joins: Option<(&'static str, &'static str)>,
    }

    impl Vocabulary for Translation {
        fn month(&self, form: MonthForm, month: u32) -> String {
            match (form, self.months) {
                (MonthForm::Wide, Some(months)) => months[month as usize - 1].to_owned(),
                _ => English.month(form, month),
            }
        }

        fn weekday(&self, abbreviated: bool, days_from_monday: u32) -> String {
            English.weekday(abbreviated, days_from_monday)
        }

        fn day_period(&self, pm: bool) -> String {
            match self.day_periods {
                Some((am, pm_text)) => if pm { pm_text } else { am }.to_owned(),
                None => English.day_period(pm),
            }
        }

        fn yesterday(&self) -> String {
            English.yesterday()
        }

        fn pattern(&self, key: PatternKey) -> String {
            self.patterns
                .iter()
                .find(|(candidate, _)| *candidate == key)
                .map_or(key.english(), |(_, pattern)| pattern)
                .to_owned()
        }

        fn join(&self, key: JoinKey) -> String {
            match (key, self.joins) {
                (JoinKey::Range, Some((range, _))) => range.to_owned(),
                (JoinKey::DateAtTime, Some((_, date_at_time))) => date_at_time.to_owned(),
                _ => English.join(key),
            }
        }
    }

    const RUSSIAN_MONTHS: [&str; 12] = [
        "января",
        "февраля",
        "марта",
        "апреля",
        "мая",
        "июня",
        "июля",
        "августа",
        "сентября",
        "октября",
        "ноября",
        "декабря",
    ];

    #[test]
    fn pattern_keys_index_the_parsed_patterns() {
        for (index, key) in PatternKey::ALL.into_iter().enumerate() {
            assert_eq!(key as usize, index);
        }
    }

    #[test]
    fn english_dates_follow_the_source_patterns() {
        let formats = DateFormats::english(HourCycle::TwentyFour);
        let day = date(2026, 10, 22);

        assert_eq!(formats.date(DateStyle::ShortDayMonth, day), "Oct 22");
        assert_eq!(formats.date(DateStyle::Short, day), "Oct 22, 2026");
        assert_eq!(
            formats.date(DateStyle::WeekdayMedium, day),
            "Thu, Oct 22, 2026"
        );
        assert_eq!(
            formats.date(DateStyle::WeekdayDayMonth, day),
            "Thursday, October 22"
        );
        assert_eq!(formats.date(DateStyle::MonthYear, day), "October 2026");
        assert_eq!(
            formats.date_time(DateStyle::WeekdayMedium, day.and_hms_opt(9, 5, 0).unwrap()),
            "Thu, Oct 22, 2026 at 09:05"
        );
    }

    #[test]
    fn translations_reorder_fields_and_inflect_month_names() {
        let chinese = DateFormats::from_vocabulary(
            &Translation {
                patterns: &[
                    (PatternKey::ShortDate, "y/M/d"),
                    (PatternKey::Date, "y年M月d日"),
                    (PatternKey::Time12, "ah:mm"),
                ],
                day_periods: Some(("上午", "下午")),
                ..Default::default()
            },
            HourCycle::Twelve,
        );
        assert_eq!(
            chinese.date(DateStyle::Short, date(2026, 10, 22)),
            "2026/10/22"
        );
        assert_eq!(
            chinese.date(DateStyle::Medium, date(2026, 10, 22)),
            "2026年10月22日"
        );
        assert_eq!(chinese.time(15, 4), "下午3:04");

        let russian = DateFormats::from_vocabulary(
            &Translation {
                patterns: &[
                    (PatternKey::Date, "d MMMM y 'г'."),
                    (PatternKey::ShortDate, "dd.MM.yy"),
                ],
                months: Some(RUSSIAN_MONTHS),
                joins: Some(("{0} – {1}", "{1}, {0}")),
                ..Default::default()
            },
            HourCycle::TwentyFour,
        );
        assert_eq!(
            russian.date(DateStyle::Medium, date(2026, 5, 2)),
            "2 мая 2026 г."
        );
        assert_eq!(russian.date(DateStyle::Short, date(2006, 5, 2)), "02.05.06");
        assert_eq!(
            russian.date_time(
                DateStyle::Medium,
                date(2026, 5, 2).and_hms_opt(7, 0, 0).unwrap()
            ),
            "07:00, 2 мая 2026 г."
        );
    }

    #[test]
    fn hour_cycles_cover_midnight_noon_and_the_end_of_a_day() {
        let twelve = DateFormats::english(HourCycle::Twelve);
        let twenty_four = DateFormats::english(HourCycle::TwentyFour);

        assert_eq!(twelve.time(0, 5), "12:05 AM");
        assert_eq!(twelve.time(12, 0), "12:00 PM");
        assert_eq!(twelve.time(13, 30), "1:30 PM");
        assert_eq!(twelve.time(24, 0), "12:00 AM");
        assert_eq!(twelve.hour(8), "8 AM");
        assert_eq!(twenty_four.time(9, 5), "09:05");
        assert_eq!(twenty_four.time(24, 0), "24:00");
        assert_eq!(twenty_four.hour(8), "08:00");
    }

    #[test]
    fn typed_times_are_read_on_either_clock() {
        let english = DateFormats::english(HourCycle::Twelve);
        for (text, expected) in [
            ("14:30", Some((14, 30))),
            ("9:05", Some((9, 5))),
            ("2:30 PM", Some((14, 30))),
            ("2:30pm", Some((14, 30))),
            ("12:15 a.m.", Some((0, 15))),
            ("12 PM", Some((12, 0))),
            ("2 pm", Some((14, 0))),
            ("2:30 pm.", Some((14, 30))),
            ("14.30", Some((14, 30))),
            ("14", None),
            ("13:00 PM", None),
            ("24:00", None),
            ("9:60", None),
            ("noon", None),
            ("", None),
        ] {
            assert_eq!(english.parse_time(text), expected, "{text:?}");
        }

        let chinese = DateFormats::from_vocabulary(
            &Translation {
                day_periods: Some(("上午", "下午")),
                ..Default::default()
            },
            HourCycle::Twelve,
        );
        assert_eq!(chinese.parse_time("下午2:30"), Some((14, 30)));
        assert_eq!(chinese.parse_time("下午0:05"), Some((12, 5)));
        assert_eq!(chinese.parse_time("上午0:05"), Some((0, 5)));
        assert_eq!(chinese.parse_time(&chinese.time(9, 30)), Some((9, 30)));
        let spanish = DateFormats::from_vocabulary(
            &Translation {
                day_periods: Some(("a. m.", "p. m.")),
                ..Default::default()
            },
            HourCycle::Twelve,
        );
        assert_eq!(spanish.parse_time("2:30 p. m."), Some((14, 30)));
        assert_eq!(spanish.parse_time(&spanish.time(23, 59)), Some((23, 59)));
    }

    #[test]
    fn date_ranges_leave_out_the_shared_month_and_year() {
        let formats = DateFormats::english(HourCycle::TwentyFour);

        assert_eq!(
            formats.date_range(date(2026, 10, 19), date(2026, 10, 25)),
            "October 19 – 25, 2026"
        );
        assert_eq!(
            formats.date_range(date(2026, 10, 26), date(2026, 11, 1)),
            "Oct 26 – Nov 1, 2026"
        );
        assert_eq!(
            formats.date_range(date(2026, 12, 28), date(2027, 1, 3)),
            "Dec 28, 2026 – Jan 3, 2027"
        );
    }

    #[test]
    fn patterns_quote_literals_and_reject_unknown_symbols() {
        assert_eq!(
            Pattern::parse("d 'de' MMMM 'de' y").unwrap().0[1],
            Field::Literal(" de ".into())
        );
        assert_eq!(
            Pattern::parse("h 'o''clock'").unwrap().0[1],
            Field::Literal(" o'clock".into())
        );
        assert_eq!(
            Pattern::parse("''").unwrap().0,
            [Field::Literal("'".into())]
        );
        assert_eq!(Pattern::parse("y-MM-dd G"), Err('G'));
        assert_eq!(Pattern::parse("ddd"), Err('d'));

        let broken = DateFormats::from_vocabulary(
            &Translation {
                patterns: &[(PatternKey::Date, "d. MMMM yyyy QQQ")],
                joins: Some(("{0}", "{0} {0}")),
                ..Default::default()
            },
            HourCycle::TwentyFour,
        );
        assert_eq!(
            broken.date(DateStyle::Medium, date(2026, 5, 2)),
            "May 2, 2026"
        );
        assert_eq!(broken.range("a", "b"), "a – b");
        assert_eq!(
            broken.date_time(
                DateStyle::Medium,
                date(2026, 5, 2).and_hms_opt(7, 0, 0).unwrap()
            ),
            "May 2, 2026 at 07:00"
        );
    }

    #[test]
    fn joins_keep_braces_inside_their_parts() {
        let join = Join::parse("{1}: {0}").unwrap();
        assert_eq!(join.apply("{1}", "{0}"), "{0}: {1}");
    }

    fn headless_app() -> AppWindow {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        struct Headless(Rc<MinimalSoftwareWindow>);
        impl slint::platform::Platform for Headless {
            fn create_window_adapter(
                &self,
            ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).unwrap();
        AppWindow::new().unwrap()
    }

    #[test]
    fn the_catalog_source_text_matches_the_english_formats() {
        let app = headless_app();
        for cycle in [HourCycle::Twelve, HourCycle::TwentyFour] {
            assert_eq!(
                DateFormats::load(&app.global::<I18n>(), cycle),
                DateFormats::english(cycle)
            );
        }

        app.set_time_format("12h".into());
        reload(&app);
        assert_eq!(current().time(13, 30), "1:30 PM");
        assert_eq!(app.get_time_format_sample(), "1:30 PM");
        app.set_time_format("24h".into());
        reload(&app);
        assert_eq!(current().time(13, 30), "13:30");
        assert_eq!(app.get_time_format_sample(), "13:30");
    }

    /// Every translated pattern must parse, or its language silently falls
    /// back to the source pattern.
    #[test]
    fn translated_patterns_use_supported_symbols() {
        let lang = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lang");
        let mut checked = 0;
        for language in std::fs::read_dir(lang).unwrap() {
            let catalog = language.unwrap().path().join("LC_MESSAGES/flectar-mail.po");
            let Ok(catalog) = std::fs::read_to_string(catalog) else {
                continue;
            };
            for entry in catalog.split("\n\n") {
                let field = |name: &str| {
                    entry
                        .lines()
                        .find_map(|line| line.strip_prefix(name))
                        .map(|value| value.trim_matches('"').replace("\\\"", "\""))
                };
                let (Some(context), Some(pattern)) = (field("msgctxt "), field("msgstr ")) else {
                    continue;
                };
                if context.contains("pattern (CLDR)") && !pattern.is_empty() {
                    assert!(Pattern::parse(&pattern).is_ok(), "{context}: {pattern}");
                    checked += 1;
                }
            }
        }
        assert!(checked > 0);
    }

    #[test]
    fn regional_clocks_follow_cldr() {
        for (locale, expected) in [
            (Some("en_US.UTF-8"), HourCycle::Twelve),
            (Some("en-GB"), HourCycle::TwentyFour),
            (Some("en"), HourCycle::Twelve),
            (Some("es_MX"), HourCycle::Twelve),
            (Some("es-419"), HourCycle::Twelve),
            (Some("es_ES.UTF-8@euro"), HourCycle::TwentyFour),
            (Some("fr-CA"), HourCycle::TwentyFour),
            (Some("en-CA"), HourCycle::Twelve),
            (Some("zh-Hant-TW"), HourCycle::Twelve),
            (Some("zh-Hans-CN"), HourCycle::TwentyFour),
            (Some("ja_JP"), HourCycle::TwentyFour),
            (Some("ko"), HourCycle::Twelve),
            (Some("ru"), HourCycle::TwentyFour),
            (Some("en-001"), HourCycle::Twelve),
            (Some("de-001"), HourCycle::TwentyFour),
            (Some("C"), HourCycle::TwentyFour),
            (None, HourCycle::TwentyFour),
        ] {
            assert_eq!(regional_hour_cycle(locale), expected, "{locale:?}");
        }
        assert!(TWELVE_HOUR_REGIONS.is_sorted());
        assert!(TWELVE_HOUR_LANGUAGES.is_sorted());
    }
}
