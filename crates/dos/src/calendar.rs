//! The proleptic Gregorian calendar, for DOS date and time.

/// A calendar date and time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// What a capsule's clock reads until the supervisor says what time it is:
/// the first moment DOS can name.
pub const DOS_EPOCH: DateTime = DateTime { year: 1980, month: 1, day: 1, hour: 0, minute: 0, second: 0 };

/// Days since 1970-01-01 (Howard Hinnant's civil-date algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

impl DateTime {
    /// This moment `seconds` later.
    pub fn plus(self, seconds: u64) -> DateTime {
        let day = days_from_civil(self.year as i64, self.month as i64, self.day as i64);
        let tod = self.hour as u64 * 3600 + self.minute as u64 * 60 + self.second as u64 + seconds;
        let (y, m, d) = civil_from_days(day + (tod / 86_400) as i64);
        let tod = tod % 86_400;
        DateTime {
            year: y.clamp(0, u16::MAX as i64) as u16,
            month: m as u8,
            day: d as u8,
            hour: (tod / 3600) as u8,
            minute: (tod % 3600 / 60) as u8,
            second: (tod % 60) as u8,
        }
    }

    /// 0 for Sunday, as INT 21h/2Ah reports it.
    pub fn weekday(&self) -> u8 {
        // 1970-01-01 was a Thursday.
        (days_from_civil(self.year as i64, self.month as i64, self.day as i64) + 4).rem_euclid(7) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> DateTime {
        DateTime { year, month, day, hour, minute, second }
    }

    #[test]
    fn time_carries_into_days_months_years() {
        assert_eq!(at(1999, 12, 31, 23, 59, 59).plus(1), at(2000, 1, 1, 0, 0, 0));
        assert_eq!(at(2000, 2, 28, 12, 0, 0).plus(86_400), at(2000, 2, 29, 12, 0, 0)); // 2000 is a leap year
        assert_eq!(at(1900, 2, 28, 0, 0, 0).plus(86_400), at(1900, 3, 1, 0, 0, 0)); // 1900 is not
        assert_eq!(DOS_EPOCH.plus(0), DOS_EPOCH);
    }

    #[test]
    fn weekdays_count_from_sunday() {
        assert_eq!(at(1980, 1, 1, 0, 0, 0).weekday(), 2); // a Tuesday
        assert_eq!(at(2031, 2, 3, 0, 0, 0).weekday(), 1);
        assert_eq!(at(2000, 2, 29, 0, 0, 0).weekday(), 2);
    }
}
