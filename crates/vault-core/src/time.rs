//! RFC 3339 の時刻（UTC・ミリ秒。例：`2026-11-16T09:30:00.123Z`）。
//!
//! 外部のライブラリを使わずに自作する。

use std::time::{SystemTime, UNIX_EPOCH};

const MS_PER_DAY: i64 = 86_400_000;

/// UNIX 時刻（1970-01-01T00:00:00Z からのミリ秒）を RFC 3339 の文字にする。
/// 年が 0000〜9999 の外なら `None`。
pub fn format_unix_millis(unix_ms: i64) -> Option<String> {
    let days = unix_ms.div_euclid(MS_PER_DAY);
    let ms_of_day = unix_ms.rem_euclid(MS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    if !(0..=9999).contains(&year) {
        return None;
    }
    let hour = ms_of_day / 3_600_000;
    let minute = ms_of_day / 60_000 % 60;
    let second = ms_of_day / 1000 % 60;
    let milli = ms_of_day % 1000;
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z"
    ))
}

/// 時刻を UNIX 時刻（ミリ秒）にする。1970 年より前は負の値（ミリ秒より細かい分は切り捨てて、過去の側に寄せる）。
pub fn unix_millis(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(e) => {
            let d = e.duration();
            let mut ms = d.as_millis();
            if d.subsec_nanos() % 1_000_000 != 0 {
                ms += 1;
            }
            i64::try_from(ms).map(|v| -v).unwrap_or(i64::MIN)
        }
    }
}

/// 今の時刻の UNIX 時刻（ミリ秒）。
pub fn now_unix_millis() -> i64 {
    unix_millis(SystemTime::now())
}

/// 1970-01-01 からの日数を、グレゴリオ暦の年・月・日にする（Howard Hinnant の days_from_civil の逆の計算）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // 0..=146096
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // 0..=399
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // 0..=365
    let mp = (5 * doy + 2) / 153; // 0..=11（3月が0）
    let day = doy - (153 * mp + 2) / 5 + 1; // 1..=31
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // 1..=12
    let year = yoe + era * 400 + i64::from(month <= 2);
    // day・month は上のとおり小さい正の数なので、u32 に収まる
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn known_instants() {
        let cases: [(i64, &str); 11] = [
            (0, "1970-01-01T00:00:00.000Z"),
            (1, "1970-01-01T00:00:00.001Z"),
            (-1, "1969-12-31T23:59:59.999Z"),
            (951_782_400_000, "2000-02-29T00:00:00.000Z"), // 400で割り切れる年のうるう日
            (1_000_000_000_123, "2001-09-09T01:46:40.123Z"),
            (1_709_164_800_000, "2024-02-29T00:00:00.000Z"),
            (4_102_444_800_000, "2100-01-01T00:00:00.000Z"),
            (4_107_542_399_999, "2100-02-28T23:59:59.999Z"), // 2100 年はうるう年でない
            (4_107_542_400_000, "2100-03-01T00:00:00.000Z"),
            (253_402_300_799_999, "9999-12-31T23:59:59.999Z"),
            (-62_167_219_200_000, "0000-01-01T00:00:00.000Z"),
        ];
        for (ms, text) in cases {
            assert_eq!(format_unix_millis(ms).as_deref(), Some(text), "{ms}");
        }
    }

    #[test]
    fn out_of_range_years_are_none() {
        assert_eq!(format_unix_millis(253_402_300_800_000), None); // 10000 年
        assert_eq!(format_unix_millis(-62_167_219_200_001), None); // -1 年
        assert_eq!(format_unix_millis(i64::MAX), None);
        assert_eq!(format_unix_millis(i64::MIN), None);
    }

    #[test]
    fn every_day_of_four_centuries_is_consecutive() {
        // 1970-01-01 から 400 年分、1日ずつ進めて、日付が正しく1日ずつ進むことを確かめる。
        // 次の日は、前の日だけから求める：月の最後の日（日数はうるう年の規則から）の次は「次の月の1日」、
        // 12月31日の次は「次の年の1月1日」
        let mut prev: (i64, u32, u32) = (1969, 12, 31);
        for day in 0..146_097 {
            let (py, pm, pd) = prev;
            let expected_next = if pd < days_in_month(py, pm) {
                (py, pm, pd + 1)
            } else if pm < 12 {
                (py, pm + 1, 1)
            } else {
                (py + 1, 1, 1)
            };
            let got = civil_from_days(day);
            assert_eq!(got, expected_next, "day {day}");
            prev = got;
        }
        assert_eq!(prev, (2369, 12, 31));
    }

    fn days_in_month(y: i64, m: u32) -> u32 {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        match m {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        }
    }

    #[test]
    fn unix_millis_of_system_time() {
        assert_eq!(unix_millis(UNIX_EPOCH), 0);
        assert_eq!(
            unix_millis(UNIX_EPOCH + Duration::from_millis(1_500)),
            1_500
        );
        assert_eq!(unix_millis(UNIX_EPOCH - Duration::from_millis(1)), -1);
        assert_eq!(unix_millis(UNIX_EPOCH - Duration::from_micros(500)), -1);
        assert!(now_unix_millis() > 1_700_000_000_000);
    }
}
