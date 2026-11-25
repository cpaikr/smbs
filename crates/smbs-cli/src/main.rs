//! `smbs` CLI: one JSON document per invocation on stdout (contract v1).
//!
//! Exit statuses: 0 success, 1 execution failure, 2 invalid invocation.
//! stderr stays empty; help and `--version` are plain text on stdout.

mod clock;

use std::{ffi::OsString, io::Write, process::ExitCode};

use clap::{
    Args, Parser, Subcommand,
    error::{ContextKind, ContextValue, ErrorKind},
};
use serde::Serialize;
use smbs::{
    Basis, CURRENCIES, Client, Currency, ErrorCode, IntervalSource, PacingConfig, Series,
    SeriesRequest, SmbsError, YearMonth, parse_interval_ms,
};

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

/// Seoul Money Brokerage (smbs.biz) monthly KRW base rates (매매기준율).
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
    MonthlyAverage(SeriesArgs),
    /// Fetch 월말 매매기준율 (month-end base rate) series.
    #[command(after_help = SERIES_AFTER)]
    MonthEnd(SeriesArgs),
    /// List supported currency codes, Korean names, and quote units (offline).
    Currencies(OutputArgs),
    /// Print the CLI version and contract version as JSON (offline).
    Version(OutputArgs),
}

#[derive(Debug, Args)]
struct SeriesArgs {
    /// Currency codes, comma-separated or repeated (e.g. USD,JPY). See `smbs currencies`.
    #[arg(
        long,
        value_name = "codes",
        value_delimiter = ',',
        required = true,
        allow_hyphen_values = true
    )]
    currency: Vec<String>,
    /// First month, YYYY-MM (1990-01 or later).
    #[arg(long, value_name = "YYYY-MM", allow_hyphen_values = true)]
    from: String,
    /// Last month, YYYY-MM. Defaults to the current month in Korea.
    #[arg(long, value_name = "YYYY-MM", allow_hyphen_values = true)]
    to: Option<String>,
    /// Milliseconds to wait before each request (0-60000; default 1000).
    #[arg(long, value_name = "ms", allow_hyphen_values = true)]
    request_interval_ms: Option<String>,
    #[command(flatten)]
    output: OutputArgs,
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
    error: SmbsError,
    metadata: Metadata,
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
}

#[derive(Serialize)]
struct VersionResult {
    name: &'static str,
    version: &'static str,
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
            run_series("monthly-average", Basis::MonthlyAverage, &args)
        }
        Command::MonthEnd(args) => run_series("month-end", Basis::MonthEnd, &args),
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
            },
            Metadata::new(None),
            vec![],
            args.pretty,
        ),
    }
}

fn run_series(operation: &'static str, basis: Basis, args: &SeriesArgs) -> ExitCode {
    let pretty = args.output.pretty;
    // Validate every input before touching the pacing state or the network.
    let prepared = prepare(basis, args).and_then(|requests| {
        let explicit = args
            .request_interval_ms
            .as_deref()
            .map(|value| parse_interval_ms(value, "request-interval-ms"))
            .transpose()?;
        PacingConfig::from_env(explicit).map(|pacing| (requests, pacing))
    });
    let (requests, pacing) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return fail(Some(operation), error, Metadata::new(None), pretty),
    };
    let client = match build_client(pacing) {
        Ok(client) => client,
        Err(error) => return fail(Some(operation), error, Metadata::new(None), pretty),
    };
    let metadata = Metadata::new(Some(client.pacing()));

    let mut series = Vec::with_capacity(requests.len());
    for request in &requests {
        match client.fetch(request) {
            Ok(fetched) => series.push(fetched),
            Err(error) => return fail(Some(operation), error, metadata, pretty),
        }
    }
    let warnings = series
        .iter()
        .filter(|s| s.observations.is_empty())
        .map(|s| {
            format!(
                "smbs.biz returned no {} rows between {} and {}.",
                s.currency, s.from, s.to
            )
        })
        .collect();
    let first = requests[0];
    let result = SeriesResult {
        basis,
        from: first.from(),
        to: first.to(),
        series,
    };
    succeed(operation, result, metadata, warnings, pretty)
}

fn prepare(basis: Basis, args: &SeriesArgs) -> Result<Vec<SeriesRequest>, SmbsError> {
    let from = YearMonth::parse(&args.from, "from")?;
    let to = match &args.to {
        Some(to) => YearMonth::parse(to, "to")?,
        None => clock::current_korea_month()?,
    };
    let mut currencies: Vec<&'static Currency> = Vec::new();
    for code in &args.currency {
        let currency = Currency::parse(code)?;
        if !currencies.contains(&currency) {
            currencies.push(currency);
        }
    }
    currencies
        .into_iter()
        .map(|currency| SeriesRequest::new(basis, currency, from, to))
        .collect()
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
    error: SmbsError,
    metadata: Metadata,
    pretty: bool,
) -> ExitCode {
    let status = if error.code == ErrorCode::InvalidRequest {
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
