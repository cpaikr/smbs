use serde::Serialize;

use crate::SmbsError;

/// A currency offered in the smbs.biz 월평균/월말 매매기준율 dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Currency {
    /// ISO-style code sent to the source, e.g. `USD`.
    pub code: &'static str,
    /// Korean display name from the source page.
    pub name: &'static str,
    /// Number of currency units one quoted KRW rate refers to (1 or 100).
    pub unit: u16,
}

const fn c(code: &'static str, name: &'static str, unit: u16) -> Currency {
    Currency { code, name, unit }
}

/// Observed 2026-11-25 on `MonAvgStdExRate.jsp` and `MonLastStdExRate.jsp`.
///
/// The source answers an unknown code with an empty series, which is
/// indistinguishable from "no data", so requests are limited to this list.
pub const CURRENCIES: &[Currency] = &[
    c("USD", "미국 달러", 1),
    c("CNH", "위안", 1),
    c("JPY", "일본 엔", 100),
    c("EUR", "유로", 1),
    c("GBP", "영국 파운드", 1),
    c("CAD", "캐나다 달러", 1),
    c("CHF", "스위스 프랑", 1),
    c("AUD", "호주 달러", 1),
    c("NZD", "뉴질랜드 달러", 1),
    c("CNY", "중국 위안", 1),
    c("HKD", "홍콩 달러", 1),
    c("TWD", "대만 달러", 1),
    c("MNT", "몽골 투그릭", 1),
    c("KZT", "카자흐스탄 텡게", 1),
    c("THB", "태국 밧", 1),
    c("SGD", "싱가포르 달러", 1),
    c("IDR", "인도네시아 루피아", 100),
    c("MYR", "말레이시아 링깃", 1),
    c("PHP", "필리핀 페소", 1),
    c("VND", "베트남 동", 100),
    c("BND", "브루나이 달러", 1),
    c("INR", "인도 루피", 1),
    c("PKR", "파키스탄 루피", 1),
    c("BDT", "방글라데시 타카", 1),
    c("KHR", "캄보디아 리엘", 1),
    c("MOP", "마카오 파타카", 1),
    c("NPR", "네팔 루피", 1),
    c("LKR", "스리랑카 루피", 1),
    c("UZS", "우즈베키스탄 숨", 1),
    c("MMK", "미얀마 짯", 1),
    c("MXN", "멕시코 페소", 1),
    c("BRL", "브라질 헤알", 1),
    c("ARS", "아르헨티나 페소", 1),
    c("CLP", "칠레 페소", 1),
    c("COP", "콜롬비아 페소", 1),
    c("SEK", "스웨덴 크로나", 1),
    c("DKK", "덴마크 크로네", 1),
    c("NOK", "노르웨이 크로네", 1),
    c("RUB", "러시아 루블", 1),
    c("HUF", "헝가리 포린트", 1),
    c("PLN", "폴란드 즈워티", 1),
    c("CZK", "체코 코루나", 1),
    c("RON", "루마니아 레우", 1),
    c("SAR", "사우디아라비아 리얄", 1),
    c("QAR", "카타르 리얄", 1),
    c("ILS", "이스라엘 셰켈", 1),
    c("JOD", "요르단 디나르", 1),
    c("KWD", "쿠웨이트 디나르", 1),
    c("BHD", "바레인 디나르", 1),
    c("AED", "아랍에미리트 디르함", 1),
    c("TRY", "튀르키예 리라", 1),
    c("OMR", "오만 리얄", 1),
    c("ZAR", "남아프리카공화국 랜드", 1),
    c("EGP", "이집트 파운드", 1),
    c("KES", "케냐 실링", 1),
    c("LYD", "리비아 디나르", 1),
    c("ETB", "에티오피아 비르", 1),
    c("FJD", "피지 달러", 1),
];

impl Currency {
    /// Resolves a code case-insensitively against [`CURRENCIES`].
    ///
    /// # Errors
    /// `invalid_request` when the code is not offered by the source.
    pub fn parse(code: &str) -> Result<&'static Self, SmbsError> {
        let trimmed = code.trim();
        CURRENCIES
            .iter()
            .find(|currency| currency.code.eq_ignore_ascii_case(trimmed))
            .ok_or_else(|| {
                SmbsError::invalid(
                    format!("Unsupported currency code \"{trimmed}\"."),
                    "currency",
                    "Run `smbs currencies` to list supported codes.",
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_uppercase_and_units_are_known() {
        let mut codes: Vec<_> = CURRENCIES.iter().map(|c| c.code).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), CURRENCIES.len());
        for currency in CURRENCIES {
            assert_eq!(currency.code.len(), 3);
            assert!(currency.code.bytes().all(|b| b.is_ascii_uppercase()));
            assert!(matches!(currency.unit, 1 | 100));
        }
        let per_hundred: Vec<_> = CURRENCIES
            .iter()
            .filter(|c| c.unit == 100)
            .map(|c| c.code)
            .collect();
        assert_eq!(per_hundred, ["JPY", "IDR", "VND"]);
    }

    #[test]
    fn parse_is_case_insensitive_and_rejects_unknown_codes() {
        assert_eq!(Currency::parse(" usd ").unwrap().code, "USD");
        for bad in ["XXX", "", "US", "USDX", "구분자"] {
            assert_eq!(
                Currency::parse(bad).unwrap_err().code,
                crate::ErrorCode::InvalidRequest
            );
        }
    }
}
