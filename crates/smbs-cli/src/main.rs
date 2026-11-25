//! `smbs` CLI: one JSON document per invocation on stdout (contract v1).
//!
//! Exit statuses: 0 success, 1 execution failure, 2 invalid invocation.
//! stderr stays empty; help and `--version` are plain text on stdout.

mod clock;
mod output;
mod table;

use std::{ffi::OsString, fmt::Display, io::Write, path::PathBuf, process::ExitCode};

use clap::{
    Args, Parser, Subcommand,
    error::{ContextKind, ContextValue, ErrorKind},
};
use output::{Artifact, OutputTarget};
use serde::Serialize;
use smbs::{
    Basis, CURRENCIES, Client, Currency, DailyBasis, DailyRequest, DailySeries, Date, ErrorCode,
    IntervalSource, PacingConfig, Series, SeriesRequest, SmbsError, YearMonth, parse_interval_ms,
};
use table::Table;

const CONTRACT_VERSION: &str = "1";

const ROOT_AFTER: &str = "\
Output:
  Each command prints one JSON document (one line unless --pretty): {\"ok\":true,\"operation\",\"result\",\"metadata\",\"warnings\"}
  or {\"ok\":false,\"operation\",\"error\":{\"code\",\"message\",\"retryable\",...}}.
  Exit status: 0 success, 1 execution failure, 2 invalid invocation.

Request pacing:
  Every smbs process on this machine waits 1000 ms before each request to
  smbs.biz, coordinated through a lock file. Override with
  --request-interval-ms or SMBS_REQUEST_INTERVAL_MS (0-60000); set
  SMBS_STATE_DIR to an absolute directory to move the lock file.";

const SERIES_AFTER: &str = "\
Examples:
  smbs monthly-average --currency USD --from 2025-01 --to 2025-12
  smbs month-end --currency USD,JPY --from 2024-01

Notes:
  - rate is KRW per `unit` units of the currency; JPY, IDR, and VND are quoted per 100.
  - The current month has no row until smbs.biz publishes it.
  - One request is sent per currency, paced; the command stops at the first failure.";

const DAILY_AFTER: &str = "\
Examples:
  smbs daily --currency USD --from 2025-01-01 --to 2025-12-31
  smbs daily --currency USD,JPY --from 2025-11-01

Notes:
  - rate is KRW per `unit` units of the currency; JPY, IDR, and VND are quoted per 100.
  - Only business days have rows; weekends and holidays are absent.
  - A range may span at most 99 years (the source labels years with two digits).
  - One request is sent per currency, paced; the command stops at the first failure.";

/// Seoul Money Brokerage (smbs.biz) KRW base rates (매매기준율): monthly and daily.
#[derive(Debug, Parser)]
#[command(
    name = "smbs",
    version,
    after_help = ROOT_AFTER,
    disable_help_subcommand = true,
    max_term_width = 100
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Fetch 월평균 매매기준율 (monthly average base rate) series.
    #[command(after_help = SERIES_AFTER)]
    MonthlyAverage(MonthlyArgs),
    /// Fetch 월말 매매기준율 (month-end base rate) series.
    #[command(after_help = SERIES_AFTER)]
    MonthEnd(MonthlyArgs),
    /// Fetch 기간별 매매기준율 (daily base rate) series.
    #[command(after_help = DAILY_AFTER)]
    Daily(DailyArgs),
    /// List supported currency codes, Korean names, and quote units (offline).
    Currencies(OutputArgs),
    /// Print the CLI version and contract version as JSON (offline).
    Version(OutputArgs),
}

#[derive(Debug, Args)]
struct MonthlyArgs {
    /// First month, YYYY-MM (1990-01 or later).
    #[arg(long, value_name = "YYYY-MM", allow_hyphen_values = true)]
    from: String,
    /// Last month, YYYY-MM. Defaults to the current month in Korea.
    #[arg(long, value_name = "YYYY-MM", allow_hyphen_values = true)]
    to: Option<String>,
    #[command(flatten)]
    fetch: FetchArgs,
}

#[derive(Debug, Args)]
struct DailyArgs {
    /// First date, YYYY-MM-DD (1990-01-01 or later).
    #[arg(long, value_name = "YYYY-MM-DD", allow_hyphen_values = true)]
    from: String,
    /// Last date, YYYY-MM-DD. Defaults to today in Korea.
    #[arg(long, value_name = "YYYY-MM-DD", allow_hyphen_values = true)]
    to: Option<String>,
    #[command(flatten)]
    fetch: FetchArgs,
}

/// Options shared by every command that contacts smbs.biz.
#[derive(Debug, Args)]
struct FetchArgs {
    /// Currency codes, comma-separated or repeated (e.g. USD,JPY). See `smbs currencies`.
    #[arg(
        long,
        value_name = "codes",
        value_delimiter = ',',
        required = true,
        allow_hyphen_values = true
    )]
    currency: Vec<String>,
    /// Milliseconds to wait before each request (0-60000; default 1000).
    #[arg(long, value_name = "ms", allow_hyphen_values = true)]
    request_interval_ms: Option<String>,
    /// Also write the rates as a table to this .csv or .xlsx file.
    #[arg(long, value_name = "path", allow_hyphen_values = true)]
    output: Option<PathBuf>,
    /// Replace the --output file if it already exists.
    #[arg(long, requires = "output")]
    overwrite: bool,
    #[command(flatten)]
    json: OutputArgs,
}

#[derive(Debug, Args)]
struct OutputArgs {
    /// Print indented JSON.
    #[arg(long)]
    pretty: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Success<T: Serialize> {
    ok: bool,
    operation: &'static str,
    result: T,
    metadata: Metadata,
    warnings: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Failure {
    ok: bool,
    operation: Option<&'static str>,
    error: CliError,
    metadata: Metadata,
}

/// The envelope's error: an SDK error, or a failure the CLI itself owns.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliError {
    code: CliErrorCode,
    message: String,
    retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery_hint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CliErrorCode {
    /// Writing the --output file failed after the rates were fetched.
    OutputFailed,
    #[serde(untagged)]
    Sdk(ErrorCode),
}

impl CliError {
    fn output_failed(message: String, recovery_hint: &str) -> Self {
        Self {
            code: CliErrorCode::OutputFailed,
            message,
            retryable: false,
            parameter: None,
            source_url: None,
            recovery_hint: Some(recovery_hint.to_owned()),
        }
    }
}

impl From<SmbsError> for CliError {
    fn from(error: SmbsError) -> Self {
        Self {
            code: CliErrorCode::Sdk(error.code),
            message: error.message,
            retryable: error.retryable,
            parameter: error.parameter,
            source_url: error.source_url,
            recovery_hint: error.recovery_hint,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    contract_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_interval_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_interval_source: Option<IntervalSource>,
}

impl Metadata {
    fn new(pacing: Option<&PacingConfig>) -> Self {
        Self {
            contract_version: CONTRACT_VERSION,
            request_interval_ms: pacing.map(|p| p.interval().as_millis()),
            request_interval_source: pacing.map(PacingConfig::source),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SeriesResult {
    basis: Basis,
    from: YearMonth,
    to: YearMonth,
    series: Vec<Series>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact: Option<Artifact>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DailyResult {
    basis: DailyBasis,
    from: Date,
    to: Date,
    series: Vec<DailySeries>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact: Option<Artifact>,
}

#[derive(Serialize)]
struct VersionResult {
    name: &'static str,
    version: &'static str,
    /// Rust target triple the binary was built for.
    target: &'static str,
}

fn main() -> ExitCode {
    let mut arguments: Vec<OsString> = std::env::args_os().collect();
    // A bare invocation is exactly `smbs --help`, rendered from the same source.
    if arguments.len() == 1 {
        arguments.push("--help".into());
    }
    let cli = match Cli::try_parse_from(&arguments) {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            return if write_stdout(&error.render().to_string()) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        Err(error) => {
            let operation = operation_name(arguments.get(1));
            return fail(
                operation,
                invocation_error(&error),
                Metadata::new(None),
                false,
            );
        }
    };

    match cli.command {
        Command::MonthlyAverage(args) => {
            run_monthly("monthly-average", Basis::MonthlyAverage, &args)
        }
        Command::MonthEnd(args) => run_monthly("month-end", Basis::MonthEnd, &args),
        Command::Daily(args) => run_daily(&args),
        Command::Currencies(args) => succeed(
            "currencies",
            CURRENCIES,
            Metadata::new(None),
            vec![],
            args.pretty,
        ),
        Command::Version(args) => succeed(
            "version",
            VersionResult {
                name: "smbs",
                version: env!("CARGO_PKG_VERSION"),
                target: env!("SMBS_BUILD_TARGET"),
            },
            Metadata::new(None),
            vec![],
            args.pretty,
        ),
    }
}

fn run_monthly(operation: &'static str, basis: Basis, args: &MonthlyArgs) -> ExitCode {
    run_fetch(
        operation,
        &args.fetch,
        |currencies| {
            let from = YearMonth::parse(&args.from, "from")?;
            let to = match &args.to {
                Some(to) => YearMonth::parse(to, "to")?,
                None => clock::current_korea_month()?,
            };
            let requests = currencies
                .iter()
                .map(|currency| SeriesRequest::new(basis, currency, from, to))
                .collect::<Result<Vec<_>, _>>()?;
            Ok((requests, (from, to)))
        },
        Client::fetch,
        |series: &Series| {
            series
                .observations
                .is_empty()
                .then(|| empty_warning(series.currency, series.from, series.to))
        },
        |series| Table::monthly(basis, series),
        |series, (from, to), artifact| SeriesResult {
            basis,
            from,
            to,
            series,
            artifact,
        },
    )
}

fn run_daily(args: &DailyArgs) -> ExitCode {
    run_fetch(
        "daily",
        &args.fetch,
        |currencies| {
            let from = Date::parse(&args.from, "from")?;
            let to = match &args.to {
                Some(to) => Date::parse(to, "to")?,
                None => clock::current_korea_date()?,
            };
            let requests = currencies
                .iter()
                .map(|currency| DailyRequest::new(currency, from, to))
                .collect::<Result<Vec<_>, _>>()?;
            Ok((requests, (from, to)))
        },
        Client::fetch_daily,
        |series: &DailySeries| {
            series
                .observations
                .is_empty()
                .then(|| empty_warning(series.currency, series.from, series.to))
        },
        Table::daily,
        |series, (from, to), artifact| DailyResult {
            basis: DailyBasis::Daily,
            from,
            to,
            series,
            artifact,
        },
    )
}

fn empty_warning(currency: &str, from: impl Display, to: impl Display) -> String {
    format!("smbs.biz returned no {currency} rows between {from} and {to}.")
}

/// Shared flow for commands that fetch one series per currency: validate every
/// input, then resolve pacing, then fetch in order, stopping at the first failure.
fn run_fetch<Request, Range, Fetched, Output: Serialize>(
    operation: &'static str,
    args: &FetchArgs,
    prepare: impl FnOnce(&[&'static Currency]) -> Result<(Vec<Request>, Range), SmbsError>,
    fetch: impl Fn(&Client, &Request) -> Result<Fetched, SmbsError>,
    warning: impl Fn(&Fetched) -> Option<String>,
    table: impl FnOnce(&[Fetched]) -> Table,
    finish: impl FnOnce(Vec<Fetched>, Range, Option<Artifact>) -> Output,
) -> ExitCode {
    let pretty = args.json.pretty;
    // Validate every input before touching the pacing state or the network.
    let prepared = parse_currencies(&args.currency)
        .and_then(|currencies| prepare(&currencies))
        .and_then(|(requests, range)| {
            let target = args
                .output
                .as_deref()
                .map(|path| OutputTarget::new(path, args.overwrite))
                .transpose()?;
            let explicit = args
                .request_interval_ms
                .as_deref()
                .map(|value| parse_interval_ms(value, "request-interval-ms"))
                .transpose()?;
            PacingConfig::from_env(explicit).map(|pacing| (requests, range, target, pacing))
        });
    let (requests, range, target, pacing) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return fail(Some(operation), error, Metadata::new(None), pretty),
    };
    let client = match build_client(pacing) {
        Ok(client) => client,
        Err(error) => return fail(Some(operation), error, Metadata::new(None), pretty),
    };
    let metadata = Metadata::new(Some(client.pacing()));

    let mut fetched = Vec::with_capacity(requests.len());
    for request in &requests {
        match fetch(&client, request) {
            Ok(series) => fetched.push(series),
            Err(error) => return fail(Some(operation), error, metadata, pretty),
        }
    }
    let warnings = fetched.iter().filter_map(&warning).collect();
    let artifact = match target.map(|target| target.write(&table(&fetched))) {
        None => None,
        Some(Ok(artifact)) => Some(artifact),
        Some(Err(error)) => return fail(Some(operation), error, metadata, pretty),
    };
    succeed(
        operation,
        finish(fetched, range, artifact),
        metadata,
        warnings,
        pretty,
    )
}

/// Resolves codes against the catalog, dropping repeats but keeping order.
fn parse_currencies(codes: &[String]) -> Result<Vec<&'static Currency>, SmbsError> {
    let mut currencies: Vec<&'static Currency> = Vec::new();
    for code in codes {
        let currency = Currency::parse(code)?;
        if !currencies.contains(&currency) {
            currencies.push(currency);
        }
    }
    Ok(currencies)
}

#[cfg(not(feature = "fixture-origin"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "same signature as the fixture build"
)]
fn build_client(pacing: PacingConfig) -> Result<Client, SmbsError> {
    Ok(Client::new(pacing))
}

#[cfg(feature = "fixture-origin")]
fn build_client(pacing: PacingConfig) -> Result<Client, SmbsError> {
    match std::env::var("SMBS_FIXTURE_ORIGIN") {
        Ok(origin) => Client::with_fixture_origin(pacing, &origin),
        Err(_) => Ok(Client::new(pacing)),
    }
}

fn operation_name(argument: Option<&OsString>) -> Option<&'static str> {
    match argument?.to_str()? {
        "monthly-average" => Some("monthly-average"),
        "daily" => Some("daily"),
        "month-end" => Some("month-end"),
        "currencies" => Some("currencies"),
        "version" => Some("version"),
        _ => None,
    }
}

fn invocation_error(error: &clap::Error) -> SmbsError {
    let rendered = error.render().to_string();
    let message = rendered
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches("error: ")
        .to_owned();
    // Report the bare argument name (`from`), the same vocabulary as domain errors.
    let parameter = match error.get(ContextKind::InvalidArg) {
        Some(ContextValue::String(argument)) => Some(bare_argument(argument)),
        Some(ContextValue::Strings(arguments)) => Some(
            arguments
                .iter()
                .map(|argument| bare_argument(argument))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        _ => None,
    };
    SmbsError {
        code: ErrorCode::InvalidRequest,
        message,
        retryable: false,
        parameter,
        source_url: None,
        recovery_hint: Some("Run `smbs --help` or `smbs <command> --help`.".to_owned()),
    }
}

/// `--from <YYYY-MM>` -> `from`; positional or unknown values pass through.
fn bare_argument(argument: &str) -> String {
    argument
        .split_whitespace()
        .next()
        .unwrap_or(argument)
        .trim_start_matches("--")
        .to_owned()
}

fn succeed<T: Serialize>(
    operation: &'static str,
    result: T,
    metadata: Metadata,
    warnings: Vec<String>,
    pretty: bool,
) -> ExitCode {
    let envelope = Success {
        ok: true,
        operation,
        result,
        metadata,
        warnings,
    };
    if write_json(&envelope, pretty) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn fail(
    operation: Option<&'static str>,
    error: impl Into<CliError>,
    metadata: Metadata,
    pretty: bool,
) -> ExitCode {
    let error = error.into();
    let status = if error.code == CliErrorCode::Sdk(ErrorCode::InvalidRequest) {
        2
    } else {
        1
    };
    // A failed write leaves the failure status unchanged: it is already nonzero.
    write_json(
        &Failure {
            ok: false,
            operation,
            error,
            metadata,
        },
        pretty,
    );
    ExitCode::from(status)
}

/// Returns whether the whole document reached stdout. A failed write cannot be
/// reported anywhere else (stderr stays empty), so callers turn it into a
/// nonzero exit status.
fn write_json(value: &impl Serialize, pretty: bool) -> bool {
    let mut rendered = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .expect("envelopes serialize");
    rendered.push('\n');
    write_stdout(&rendered)
}

fn write_stdout(text: &str) -> bool {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(text.as_bytes()).is_ok() && stdout.flush().is_ok()
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use smbs::{DEFAULT_INTERVAL_MS, MAX_INTERVAL_MS};

    use super::*;

    /// Help text is hand-written; keep its numbers tied to the SDK constants.
    #[test]
    fn help_matches_pacing_constants() {
        let root = Cli::command().render_long_help().to_string();
        let series = Cli::command()
            .find_subcommand_mut("month-end")
            .unwrap()
            .render_long_help()
            .to_string();
        for text in [&root, &series] {
            assert!(
                text.contains(&format!("{DEFAULT_INTERVAL_MS} ms"))
                    || text.contains(&format!("default {DEFAULT_INTERVAL_MS}"))
            );
            assert!(text.contains(&format!("0-{MAX_INTERVAL_MS}")));
        }
    }

    #[test]
    fn bare_argument_strips_flag_syntax() {
        assert_eq!(bare_argument("--from <YYYY-MM>"), "from");
        assert_eq!(bare_argument("--pretty"), "pretty");
        assert_eq!(bare_argument("-5"), "-5");
    }
}
