use std::time::{SystemTime, UNIX_EPOCH};

use smbs::{Date, SmbsError, YearMonth};

/// Korea has used UTC+9 without daylight saving since 1988.
const KST_OFFSET_SECONDS: u64 = 9 * 3600;

/// Today in Korea, the source's reference time zone.
///
/// # Errors
/// `invalid_request` naming `to` when the system clock is outside 1970-9999,
/// so the caller can pass `--to` explicitly.
pub fn current_korea_date() -> Result<Date, SmbsError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| date_of_unix_seconds(elapsed.as_secs() + KST_OFFSET_SECONDS))
        .ok_or_else(|| SmbsError {
            code: smbs::ErrorCode::InvalidRequest,
            message: "The system clock is out of range, so the default --to is unknown.".to_owned(),
            retryable: false,
            parameter: Some("to".to_owned()),
            source_url: None,
            recovery_hint: Some("Pass --to explicitly.".to_owned()),
        })
}

/// The current calendar month in Korea.
///
/// # Errors
/// As [`current_korea_date`].
pub fn current_korea_month() -> Result<YearMonth, SmbsError> {
    let today = current_korea_date()?;
    YearMonth::new(today.year(), today.month())
}

/// Converts seconds since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn date_of_unix_seconds(seconds: u64) -> Option<Date> {
    let days = i64::try_from(seconds / 86_400).ok()? + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    Date::new(
        u16::try_from(year).ok()?,
        u8::try_from(month).ok()?,
        u8::try_from(day).ok()?,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_known_instants() {
        let date = |seconds| date_of_unix_seconds(seconds).unwrap().to_string();
        assert_eq!(date(0), "1970-01-01");
        // 2024-02-29T23:59:59Z and the following second.
        assert_eq!(date(1_709_251_199), "2024-02-29");
        assert_eq!(date(1_709_251_200), "2024-03-01");
        // 2026-11-30T15:00:00Z is already 2026-12-01 in Korea.
        assert_eq!(date(1_796_050_800 + KST_OFFSET_SECONDS), "2026-12-01");
        assert_eq!(date(1_796_050_800), "2026-11-30");
        // Beyond year 9999 there is no representable date.
        assert_eq!(date_of_unix_seconds(u64::MAX / 2), None);
    }
}
