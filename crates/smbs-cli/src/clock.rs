use std::time::{SystemTime, UNIX_EPOCH};

use smbs::{SmbsError, YearMonth};

/// Korea has used UTC+9 without daylight saving since 1988.
const KST_OFFSET_SECONDS: u64 = 9 * 3600;

/// The current calendar month in Korea, the source's reference time zone.
///
/// # Errors
/// `invalid_request` naming `to` when the system clock is outside 1970-9999,
/// so the caller can pass `--to` explicitly.
pub fn current_korea_month() -> Result<YearMonth, SmbsError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| month_of_unix_seconds(elapsed.as_secs() + KST_OFFSET_SECONDS))
        .ok_or_else(|| SmbsError {
            code: smbs::ErrorCode::InvalidRequest,
            message: "The system clock is out of range, so the default --to month is unknown."
                .to_owned(),
            retryable: false,
            parameter: Some("to".to_owned()),
            source_url: None,
            recovery_hint: Some("Pass --to YYYY-MM explicitly.".to_owned()),
        })
}

/// Converts seconds since 1970-01-01 to a civil month (Howard Hinnant's algorithm).
fn month_of_unix_seconds(seconds: u64) -> Option<YearMonth> {
    let days = i64::try_from(seconds / 86_400).ok()? + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    YearMonth::new(u16::try_from(year).ok()?, u8::try_from(month).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_known_instants() {
        let month = |seconds| month_of_unix_seconds(seconds).unwrap().to_string();
        assert_eq!(month(0), "1970-01");
        // 2024-02-29T23:59:59Z and the following second.
        assert_eq!(month(1_709_251_199), "2024-02");
        assert_eq!(month(1_709_251_200), "2024-03");
        // 2026-11-30T15:00:00Z is already December in Korea.
        assert_eq!(month(1_796_050_800 + KST_OFFSET_SECONDS), "2026-12");
        assert_eq!(month(1_796_050_800), "2026-11");
        // Beyond year 9999 there is no representable month.
        assert_eq!(month_of_unix_seconds(u64::MAX / 2), None);
    }
}
