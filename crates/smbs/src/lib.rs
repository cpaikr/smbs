//! Read-only access to Seoul Money Brokerage Services (smbs.biz) KRW
//! base-rate series: 월평균 매매기준율 (monthly average), 월말 매매기준율
//! (month-end), and 기간별 매매기준율 (daily).
//!
//! Requests are validated before any network access and paced across local
//! processes (see [`PacingConfig`]).

mod chart;
mod client;
mod currency;
mod daily;
mod date;
mod error;
mod month;
mod pacing;
mod series;
mod transport;

pub use client::Client;
pub use currency::{CURRENCIES, Currency};
pub use daily::{DailyBasis, DailyObservation, DailyRequest, DailySeries};
pub use date::Date;
pub use error::{ErrorCode, SmbsError};
pub use month::YearMonth;
pub use pacing::{
    DEFAULT_INTERVAL_MS, INTERVAL_ENV, IntervalSource, MAX_INTERVAL_MS, PacingConfig,
    STATE_DIR_ENV, parse_interval_ms,
};
pub use series::{Basis, Observation, Series, SeriesRequest};
