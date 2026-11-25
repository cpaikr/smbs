use crate::{
    DailyRequest, DailySeries, PacingConfig, Series, SeriesRequest, SmbsError,
    daily::{DAILY_PATH, parse_daily_chart},
    series::parse_chart,
    transport::{DAILY_BODY_LIMIT, MONTHLY_BODY_LIMIT, SMBS_ORIGIN, Transport},
};

/// Blocking smbs.biz client. Every request is paced through [`PacingConfig`].
#[derive(Debug)]
pub struct Client {
    transport: Transport,
    pacing: PacingConfig,
}

impl Client {
    pub fn new(pacing: PacingConfig) -> Self {
        Self {
            transport: Transport::new(SMBS_ORIGIN.to_owned()),
            pacing,
        }
    }

    /// Sends requests to a loopback HTTP origin instead of smbs.biz.
    ///
    /// # Errors
    /// `invalid_request` unless `origin` is `http://127.0.0.1:<port>` or
    /// `http://localhost:<port>` with no path.
    #[cfg(feature = "fixture-origin")]
    pub fn with_fixture_origin(pacing: PacingConfig, origin: &str) -> Result<Self, SmbsError> {
        let loopback = ["http://127.0.0.1:", "http://localhost:"]
            .iter()
            .find_map(|prefix| origin.strip_prefix(prefix))
            .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()));
        if !loopback {
            return Err(SmbsError::invalid(
                "The fixture origin must be http://127.0.0.1:<port> or http://localhost:<port>.",
                "fixtureOrigin",
                "Point it at a local fixture server.",
            ));
        }
        Ok(Self {
            transport: Transport::new(origin.to_owned()),
            pacing,
        })
    }

    pub fn pacing(&self) -> &PacingConfig {
        &self.pacing
    }

    /// Fetches one currency's monthly series.
    ///
    /// # Errors
    /// Source errors (`source_unavailable`, `source_changed`,
    /// `source_parse_failure`) or `internal_error` when pacing state fails.
    pub fn fetch(&self, request: &SeriesRequest) -> Result<Series, SmbsError> {
        let (source_url, text) = self.get(
            request.basis().path(),
            &request.arr_value(),
            MONTHLY_BODY_LIMIT,
        )?;
        let observations = parse_chart(&text, request, &source_url)?;
        let currency = request.currency();
        Ok(Series {
            basis: request.basis(),
            currency: currency.code,
            name: currency.name,
            unit: currency.unit,
            from: request.from(),
            to: request.to(),
            observations,
            source_url,
        })
    }

    /// Fetches one currency's daily series (기간별 매매기준율).
    ///
    /// # Errors
    /// As [`Client::fetch`].
    pub fn fetch_daily(&self, request: &DailyRequest) -> Result<DailySeries, SmbsError> {
        let (source_url, text) = self.get(DAILY_PATH, &request.arr_value(), DAILY_BODY_LIMIT)?;
        let observations = parse_daily_chart(&text, request, &source_url)?;
        let currency = request.currency();
        Ok(DailySeries {
            currency: currency.code,
            name: currency.name,
            unit: currency.unit,
            from: request.from(),
            to: request.to(),
            observations,
            source_url,
        })
    }

    /// One paced request: the pacing lock is held until the body is fully read.
    fn get(
        &self,
        path: &str,
        arr_value: &str,
        body_limit: u64,
    ) -> Result<(String, String), SmbsError> {
        let permit = self.pacing.acquire()?;
        let response = self.transport.get(path, arr_value, body_limit);
        drop(permit);
        response
    }
}
