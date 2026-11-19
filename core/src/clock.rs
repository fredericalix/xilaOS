//! The time and the date.
//!
//! A PC keeps them in a small clock that runs on its own battery while the
//! machine is off. The kernel reads it when it starts, and now and then
//! afterwards; between two readings the timer counts. This module turns
//! what that clock holds into a moment, and moments into text and back
//! into numbers.
//!
//! The clock of the machine is believed as it is: no time zone, no summer
//! time. It tells the time its owner set it to, and that is the time
//! XilaOS shows.
//!
//! A moment is counted in seconds since the start of the year 2000, which
//! is all that programs need to compare two of them or to wait for one.

use core::fmt::Write;

use crate::shell::Text;

/// A day and a time of day.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Moment {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

pub const DAY: u64 = 86_400;

/// What the clock of a PC holds, as read from it: seconds, minutes,
/// hours, day of the month, month, year in its century, and the register
/// that says how they are written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Raw {
    pub second: u8,
    pub minute: u8,
    pub hour: u8,
    pub day: u8,
    pub month: u8,
    pub year: u8,
    /// Bit 2: plain numbers, not two decimal digits in a byte. Bit 1: hours
    /// from 0 to 23, not from 1 to 12 with a bit for the afternoon.
    pub form: u8,
}

fn leap(year: u32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in(year: u32, month: u32) -> u32 {
    match month {
        2 => 28 + leap(year) as u32,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Moment {
    /// The moment a clock holds. None if what it holds is no date: a clock
    /// whose battery died holds anything.
    pub fn read(raw: Raw) -> Option<Moment> {
        // two decimal digits in a byte, unless the clock says otherwise
        let plain = raw.form & 4 != 0;
        let number = |byte: u8| -> Option<u32> {
            match plain {
                true => Some(byte as u32),
                false => (byte & 0x0f < 10 && byte >> 4 < 10).then_some((byte >> 4) as u32 * 10 + (byte & 0x0f) as u32),
            }
        };
        let mut hour = number(raw.hour & 0x7f)?;
        if raw.form & 2 == 0 {
            // twelve hours and a bit for the afternoon: 12 is the hour after 11
            if !(1..=12).contains(&hour) {
                return None;
            }
            hour = hour % 12 + if raw.hour & 0x80 != 0 { 12 } else { 0 };
        }
        // the clock keeps two digits of the year: this century
        let moment = Moment { year: 2000 + number(raw.year)?, month: number(raw.month)?, day: number(raw.day)?, hour, minute: number(raw.minute)?, second: number(raw.second)? };
        moment.valid().then_some(moment)
    }

    fn valid(&self) -> bool {
        (2000..2100).contains(&self.year)
            && (1..=12).contains(&self.month)
            && (1..=days_in(self.year, self.month)).contains(&self.day)
            && self.hour < 24
            && self.minute < 60
            && self.second < 60
    }

    /// Seconds from the start of the year 2000 to this moment.
    pub fn seconds(&self) -> u64 {
        let mut days = (self.day - 1) as u64;
        for year in 2000..self.year {
            days += 365 + leap(year) as u64;
        }
        for month in 1..self.month {
            days += days_in(self.year, month) as u64;
        }
        days * DAY + (self.hour * 3600 + self.minute * 60 + self.second) as u64
    }

    /// The moment that many seconds after the start of the year 2000.
    pub fn at(seconds: u64) -> Moment {
        let (mut days, rest) = (seconds / DAY, (seconds % DAY) as u32);
        let mut year = 2000;
        while days >= 365 + leap(year) as u64 {
            days -= 365 + leap(year) as u64;
            year += 1;
        }
        let mut month = 1;
        while days >= days_in(year, month) as u64 {
            days -= days_in(year, month) as u64;
            month += 1;
        }
        Moment { year, month, day: days as u32 + 1, hour: rest / 3600, minute: rest / 60 % 60, second: rest % 60 }
    }

    /// The day of the week: 0 is Monday.
    pub fn weekday(&self) -> usize {
        // the first day of the year 2000 was a Saturday
        ((self.seconds() / DAY + 5) % 7) as usize
    }

    /// The date, as it sorts: `2026-11-18`.
    pub fn date(&self) -> Text {
        let mut text = Text::new();
        let _ = write!(text, "{:04}-{:02}-{:02}", self.year, self.month, self.day);
        text
    }

    /// The date and the time, as they sort: `2026-11-18 14:32:07`.
    pub fn text(&self) -> Text {
        let mut text = self.date();
        let _ = write!(text, " {:02}:{:02}:{:02}", self.hour, self.minute, self.second);
        text
    }
}

pub const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
pub const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// A time of day as people write it, `8:00` or `17:45`, in minutes since
/// midnight.
pub fn time_of_day(text: &[u8]) -> Option<u32> {
    let colon = text.iter().position(|byte| *byte == b':')?;
    let (hour, minute) = (&text[..colon], &text[colon + 1..]);
    if hour.is_empty() || hour.len() > 2 || minute.len() != 2 {
        return None;
    }
    let (hour, minute) = (crate::text::parse_number(hour)?, crate::text::parse_number(minute)?);
    (hour < 24 && minute < 60).then_some((hour * 60 + minute) as u32)
}

/// The first moment after `now` when the clock shows that time of day,
/// given in minutes since midnight: later today, or tomorrow.
pub fn next_at(now: u64, minutes: u32) -> u64 {
    let today = now / DAY * DAY + minutes as u64 * 60;
    if today > now {
        today
    } else {
        today + DAY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moment(year: u32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> Moment {
        Moment { year, month, day, hour, minute, second }
    }

    #[test]
    fn the_clock_of_a_pc_is_read_in_its_two_forms() {
        // as most firmware leaves it: decimal digits, 24 hours
        let raw = Raw { second: 0x07, minute: 0x32, hour: 0x14, day: 0x18, month: 0x11, year: 0x26, form: 2 };
        assert_eq!(Moment::read(raw), Some(moment(2026, 11, 18, 14, 32, 7)));
        // plain numbers
        let plain = Raw { second: 7, minute: 32, hour: 14, day: 18, month: 11, year: 26, form: 6 };
        assert_eq!(Moment::read(plain), Some(moment(2026, 11, 18, 14, 32, 7)));
        // twelve hours: 2 in the afternoon, noon, midnight
        for (hour, wanted) in [(0x82, 14), (0x12, 0), (0x92, 12), (0x11, 11), (0x81, 13)] {
            assert_eq!(Moment::read(Raw { hour, form: 0, ..raw }).map(|moment| moment.hour), Some(wanted), "{hour:x}");
        }
        // a clock that holds no date
        for bad in [Raw { month: 0x13, ..raw }, Raw { day: 0x31, ..raw }, Raw { second: 0x7a, ..raw }, Raw { hour: 0x24, ..raw }, Raw { hour: 0, form: 0, ..raw }, Raw { day: 0, ..raw }] {
            assert_eq!(Moment::read(bad), None, "{bad:?}");
        }
        assert_eq!(Moment::read(Raw { second: 0xff, minute: 0xff, hour: 0xff, day: 0xff, month: 0xff, year: 0xff, form: 0xff }), None);
        // the last day of February exists every four years
        assert!(Moment::read(Raw { day: 0x29, month: 0x02, year: 0x28, ..raw }).is_some());
        assert!(Moment::read(Raw { day: 0x29, month: 0x02, year: 0x27, ..raw }).is_none());
    }

    #[test]
    fn moments_are_counted_in_seconds_and_back() {
        assert_eq!(moment(2000, 1, 1, 0, 0, 0).seconds(), 0);
        assert_eq!(moment(2000, 3, 1, 0, 0, 0).seconds(), 60 * DAY);
        assert_eq!(moment(2001, 1, 1, 0, 0, 0).seconds(), 366 * DAY);
        // known from elsewhere: 2026-11-18 is 9818 days after 2000-01-01
        assert_eq!(moment(2026, 11, 18, 14, 32, 7).seconds(), 9818 * DAY + 14 * 3600 + 32 * 60 + 7);
        // every day for a century comes back as it was, one after the other
        let mut last = moment(1999, 12, 31, 0, 0, 0);
        for day in 0..36_525u64 {
            let found = Moment::at(day * DAY + 3723);
            assert_eq!(found.seconds(), day * DAY + 3723);
            assert_eq!((found.hour, found.minute, found.second), (1, 2, 3));
            assert!((found.year, found.month, found.day) > (last.year, last.month, last.day));
            last = found;
        }
        assert_eq!((last.year, last.month, last.day), (2099, 12, 31));
    }

    #[test]
    fn dates_are_written_so_that_they_sort() {
        let then = moment(2026, 11, 18, 4, 5, 6);
        assert_eq!(then.date().bytes(), b"2026-11-18");
        assert_eq!(then.text().bytes(), b"2026-11-18 04:05:06");
        // a Wednesday; and days known from a calendar
        assert_eq!(WEEKDAYS[then.weekday()], "Wednesday");
        assert_eq!(WEEKDAYS[moment(2000, 1, 1, 0, 0, 0).weekday()], "Saturday");
        assert_eq!(WEEKDAYS[moment(2024, 2, 29, 12, 0, 0).weekday()], "Thursday");
        assert_eq!(WEEKDAYS[moment(2027, 1, 1, 0, 0, 0).weekday()], "Friday");
    }

    #[test]
    fn a_time_of_day_is_read_and_waited_for() {
        assert_eq!(time_of_day(b"8:00"), Some(480));
        assert_eq!(time_of_day(b"08:05"), Some(485));
        assert_eq!(time_of_day(b"23:59"), Some(1439));
        assert_eq!(time_of_day(b"0:00"), Some(0));
        for bad in ["24:00", "8:60", "8", "8:5", ":30", "8:000", "eight:00", "8:00:00", "123:00"] {
            assert_eq!(time_of_day(bad.as_bytes()), None, "{bad}");
        }
        // 14:32:07 on some day: 17:00 is today, 8:00 and 14:32 are tomorrow
        let now = moment(2026, 11, 18, 14, 32, 7).seconds();
        assert_eq!(Moment::at(next_at(now, 17 * 60)), moment(2026, 11, 18, 17, 0, 0));
        assert_eq!(Moment::at(next_at(now, 8 * 60)), moment(2026, 11, 19, 8, 0, 0));
        assert_eq!(Moment::at(next_at(now, 14 * 60 + 32)), moment(2026, 11, 19, 14, 32, 0));
        assert_eq!(Moment::at(next_at(now, 14 * 60 + 33)), moment(2026, 11, 18, 14, 33, 0));
        // at the very second, the next one is a day later
        let sharp = moment(2026, 12, 31, 8, 0, 0).seconds();
        assert_eq!(Moment::at(next_at(sharp, 480)), moment(2027, 1, 1, 8, 0, 0));
    }
}
