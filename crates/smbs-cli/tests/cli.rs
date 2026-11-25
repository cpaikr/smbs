//! Process-level contract tests against a loopback fixture server.
//! Run with `cargo test --workspace --all-features`.
#![cfg(feature = "fixture-origin")]

use std::{
    fmt::Write as _,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::PathBuf,
    process::{Command, Output},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde_json::Value;

type Responder = dyn Fn(&str) -> Vec<u8> + Send + Sync;

/// Serves one canned HTTP response per connection and records request paths.
struct Fixture {
    origin: String,
    requests: Arc<Mutex<Vec<(Instant, String)>>>,
}

impl Fixture {
    fn start(responder: impl Fn(&str) -> Vec<u8> + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let responder: Arc<Responder> = Arc::new(responder);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap() <= 2 {
                        break;
                    }
                }
                let path = request_line
                    .split(' ')
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                recorded
                    .lock()
                    .unwrap()
                    .push((Instant::now(), path.clone()));
                let _ = stream.write_all(&responder(&path));
                // Close gracefully: dropping a socket right after a large write
                // can reset the connection before the client reads everything.
                let _ = stream.shutdown(std::net::Shutdown::Write);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = std::io::copy(&mut reader, &mut std::io::sink());
            }
        });
        Self { origin, requests }
    }

    fn paths(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|(_, p)| p.clone())
            .collect()
    }
}

fn response(status: &str, content_type: &str, extra_headers: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{extra_headers}Connection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

/// Fictional rates in the source's chart shape, EUC-KR encoded.
fn chart(caption: &str, rows: &[(&str, &str)]) -> Vec<u8> {
    let rows = rows.iter().fold(String::new(), |mut out, (label, value)| {
        let _ = write!(out, "\t\t\t<set label='{label}' value='{value}' />\r\n");
        out
    });
    let text = format!(
        "\r\n\r\n\r\n \r\n<?xml version=\"1.0\" encoding=\"EUC-KR\"?>   \r\n<chart \r\n\tshowToolTip='1'\r\n\tcaption='{caption}' \r\n\tyAxisName='Avg Rate (Won)' \r\n\t>\r\n{rows}<styles>\r\n<definition>\r\n<style name='CaptionFont' type='font' size='12' color='666666'>\r\n</style>\r\n</definition>\r\n</styles>\r\n</chart>\r\n"
    );
    encoding_rs::EUC_KR.encode(&text).0.into_owned()
}

fn xml(body: &[u8]) -> Vec<u8> {
    response("200 OK", "text/xml; charset=EUC-KR", "", body)
}

fn state_dir(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("smbs-cli-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    directory
}

fn smbs(
    fixture: Option<&Fixture>,
    state: &std::path::Path,
    interval: Option<&str>,
    args: &[&str],
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_smbs"));
    command
        .args(args)
        .env_remove("SMBS_REQUEST_INTERVAL_MS")
        .env_remove("SMBS_FIXTURE_ORIGIN")
        .env("SMBS_STATE_DIR", state)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(fixture) = fixture {
        command.env("SMBS_FIXTURE_ORIGIN", &fixture.origin);
    }
    if let Some(interval) = interval {
        command.env("SMBS_REQUEST_INTERVAL_MS", interval);
    }
    command
}

fn run(command: &mut Command) -> (Output, Value) {
    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    assert!(
        stdout.ends_with('\n'),
        "stdout must end with a newline: {stdout:?}"
    );
    assert_eq!(stdout.matches('\n').count(), 1, "one JSON line: {stdout:?}");
    assert!(
        output.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = serde_json::from_str(&stdout).unwrap();
    (output, value)
}

#[test]
fn bare_invocation_is_byte_identical_to_help() {
    let bare = smbs(None, &state_dir("bare"), None, &[]).output().unwrap();
    let help = smbs(None, &state_dir("bare"), None, &["--help"])
        .output()
        .unwrap();
    assert_eq!(bare.status.code(), Some(0));
    assert_eq!(bare.stdout, help.stdout);
    assert!(bare.stderr.is_empty());
    let text = String::from_utf8(bare.stdout).unwrap();
    for command in [
        "monthly-average",
        "month-end",
        "daily",
        "currencies",
        "version",
    ] {
        assert!(text.contains(command), "{command} missing from help");
    }
}

#[test]
fn month_end_returns_paced_series_for_each_currency() {
    let fixture = Fixture::start(|path| {
        assert!(
            path.starts_with("/ExRate/MonLastStdExRate_xml.jsp?arr_value="),
            "{path}"
        );
        let rows: &[(&str, &str)] = if path.contains("JPY") {
            &[("2025.01", "900.5"), ("2025.02", "901.25")]
        } else {
            &[("2025.01", "1000.5"), ("2025.02", "1001.25")]
        };
        xml(&chart("월말 매매기준율", rows))
    });
    let started = Instant::now();
    let (output, value) = run(&mut smbs(
        Some(&fixture),
        &state_dir("month-end"),
        Some("200"),
        &[
            "month-end",
            "--currency",
            "usd,JPY",
            "--currency",
            "USD",
            "--from",
            "2025-01",
            "--to",
            "2025-03",
        ],
    ));
    assert_eq!(output.status.code(), Some(0), "{value}");
    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "two paced requests"
    );
    assert_eq!(
        fixture.paths(),
        [
            "/ExRate/MonLastStdExRate_xml.jsp?arr_value=USD_2025-01_2025-04",
            "/ExRate/MonLastStdExRate_xml.jsp?arr_value=JPY_2025-01_2025-04",
        ]
    );
    assert_eq!(value["ok"], true);
    assert_eq!(value["operation"], "month-end");
    assert_eq!(value["metadata"]["requestIntervalMs"], 200);
    assert_eq!(value["metadata"]["requestIntervalSource"], "environment");
    let result = &value["result"];
    assert_eq!(result["basis"], "month_end");
    assert_eq!(
        (result["from"].as_str(), result["to"].as_str()),
        (Some("2025-01"), Some("2025-03"))
    );
    let series = result["series"].as_array().unwrap();
    assert_eq!(series.len(), 2);
    assert_eq!(series[0]["currency"], "USD");
    assert_eq!(series[0]["unit"], 1);
    assert_eq!(
        series[0]["sourceUrl"],
        "http://www.smbs.biz/ExRate/MonLastStdExRate_xml.jsp?arr_value=USD_2025-01_2025-04"
    );
    assert_eq!(
        series[0]["observations"],
        serde_json::json!([
            {"month": "2025-01", "rate": 1000.5},
            {"month": "2025-02", "rate": 1001.25},
        ])
    );
    assert_eq!(series[1]["currency"], "JPY");
    assert_eq!(series[1]["unit"], 100);
    assert_eq!(series[1]["name"], "일본 엔");
}

#[test]
fn empty_series_succeeds_with_a_warning() {
    // Also covers the application/xml media type, which is accepted like text/xml.
    let fixture = Fixture::start(|_| {
        response(
            "200 OK",
            "application/xml; charset=EUC-KR",
            "",
            &chart("월평균 매매기준율", &[]),
        )
    });
    let (output, value) = run(&mut smbs(
        Some(&fixture),
        &state_dir("empty"),
        Some("0"),
        &[
            "monthly-average",
            "--currency",
            "EUR",
            "--from",
            "2025-01",
            "--to",
            "2025-01",
        ],
    ));
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        value["result"]["series"][0]["observations"],
        serde_json::json!([])
    );
    assert_eq!(value["warnings"].as_array().unwrap().len(), 1);
    assert_eq!(
        fixture.paths(),
        ["/ExRate/MonAvgStdExRate_xml.jsp?arr_value=EUR_2025-01_2025-01"]
    );
}

#[test]
fn invalid_invocations_exit_2_without_requests() {
    let fixture = Fixture::start(|_| panic!("no request expected"));
    let cases: &[(&[&str], Option<&str>, Option<&str>)] = &[
        (
            &["month-end", "--currency", "XXX", "--from", "2025-01"],
            None,
            Some("currency"),
        ),
        (
            &["month-end", "--currency", "USD", "--from", "2025-1"],
            None,
            Some("from"),
        ),
        (
            &[
                "month-end",
                "--currency",
                "USD",
                "--from",
                "2025-05",
                "--to",
                "2025-01",
            ],
            None,
            Some("from"),
        ),
        (
            &[
                "month-end",
                "--currency",
                "USD",
                "--from",
                "1989-12",
                "--to",
                "1990-05",
            ],
            None,
            Some("from"),
        ),
        (
            &[
                "month-end",
                "--currency",
                "USD",
                "--from",
                "2025-01",
                "--request-interval-ms=-5",
            ],
            None,
            Some("request-interval-ms"),
        ),
        (
            &[
                "month-end",
                "--currency",
                "USD",
                "--from",
                "2025-01",
                "--request-interval-ms",
                "-5",
            ],
            None,
            Some("request-interval-ms"),
        ),
        (
            &["month-end", "--currency", "-USD", "--from", "2025-01"],
            None,
            Some("currency"),
        ),
        (
            &["month-end", "--currency", "USD,", "--from", "2025-01"],
            None,
            Some("currency"),
        ),
        (
            &["month-end", "--currency", "USD", "--from", "2025-01"],
            Some("70000"),
            Some("SMBS_REQUEST_INTERVAL_MS"),
        ),
        (&["month-end", "--currency", "USD"], None, Some("from")),
        (&["no-such-command"], None, None),
    ];
    for (args, interval, parameter) in cases {
        let (output, value) = run(&mut smbs(
            Some(&fixture),
            &state_dir("invalid"),
            *interval,
            args,
        ));
        assert_eq!(output.status.code(), Some(2), "{args:?}: {value}");
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "invalid_request");
        assert_eq!(value["error"]["retryable"], false);
        assert_eq!(
            value["error"]["parameter"].as_str(),
            *parameter,
            "{args:?}: {value}"
        );
    }
    assert!(fixture.paths().is_empty());
}

#[test]
fn source_failures_are_classified_and_exit_1() {
    let big = vec![b' '; (1 << 20) + 1];
    let html = encoding_rs::EUC_KR
        .encode("\r\n잘못된 접근입니다.<br/>")
        .0
        .into_owned();
    let cases: Vec<(Vec<u8>, &str, bool)> = vec![
        (
            response("200 OK", "text/html; charset=EUC-KR", "", &html),
            "source_changed",
            false,
        ),
        (
            xml(&chart("월평균 매매기준율", &[])),
            "source_changed",
            false,
        ),
        (xml(b"<html>not xml"), "source_parse_failure", false),
        (xml(&big), "source_changed", false),
        (
            response(
                "503 Service Unavailable",
                "text/html",
                "Retry-After: 120\r\n",
                b"",
            ),
            "rate_limited",
            true,
        ),
        (
            response(
                "302 Found",
                "text/html",
                "Location: /Member/Login.jsp\r\n",
                b"",
            ),
            "source_changed",
            false,
        ),
        (
            response("500 Internal Server Error", "text/html", "", b""),
            "source_unavailable",
            true,
        ),
    ];
    for (index, (reply, code, retryable)) in cases.into_iter().enumerate() {
        let fixture = Fixture::start(move |_| reply.clone());
        let (output, value) = run(&mut smbs(
            Some(&fixture),
            &state_dir("failures"),
            Some("0"),
            &[
                "month-end",
                "--currency",
                "USD",
                "--from",
                "2025-01",
                "--to",
                "2025-02",
            ],
        ));
        assert_eq!(output.status.code(), Some(1), "case {index}: {value}");
        assert_eq!(value["error"]["code"], code, "case {index}: {value}");
        assert_eq!(value["error"]["retryable"], retryable, "case {index}");
        assert_eq!(
            value["error"]["sourceUrl"],
            "http://www.smbs.biz/ExRate/MonLastStdExRate_xml.jsp?arr_value=USD_2025-01_2025-03"
        );
        if index == 4 {
            let hint = value["error"]["recoveryHint"].as_str().unwrap();
            assert!(hint.contains("120 seconds"), "{hint}");
        }
    }
}

#[test]
fn concurrent_processes_share_one_pacing_lock() {
    let fixture = Fixture::start(|_| xml(&chart("월평균 매매기준율", &[("2025.01", "1.5")])));
    // Every child shares this one SMBS_STATE_DIR, cleared once up front.
    let state = state_dir("shared");
    let spawn = || {
        smbs(
            Some(&fixture),
            &state,
            Some("300"),
            &[
                "monthly-average",
                "--currency",
                "USD",
                "--from",
                "2025-01",
                "--to",
                "2025-01",
            ],
        )
        .spawn()
        .unwrap()
    };
    let children: Vec<_> = (0..3).map(|_| spawn()).collect();
    for child in children {
        assert!(child.wait_with_output().unwrap().status.success());
    }
    let mut arrivals: Vec<Instant> = fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|(at, _)| *at)
        .collect();
    arrivals.sort();
    assert_eq!(arrivals.len(), 3);
    for pair in arrivals.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(gap >= Duration::from_millis(280), "requests {gap:?} apart");
    }
}

#[test]
fn offline_commands_need_no_source() {
    let (output, value) = run(&mut smbs(
        None,
        &state_dir("offline"),
        Some("bogus"),
        &["currencies"],
    ));
    assert_eq!(output.status.code(), Some(0));
    let currencies = value["result"].as_array().unwrap();
    assert_eq!(currencies.len(), 58);
    assert_eq!(
        currencies[2],
        serde_json::json!({"code": "JPY", "name": "일본 엔", "unit": 100})
    );

    let (output, value) = run(&mut smbs(None, &state_dir("offline"), None, &["version"]));
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(value["result"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(value["result"]["target"].as_str().unwrap().contains('-'));
    assert_eq!(
        value["metadata"],
        serde_json::json!({"contractVersion": "1"})
    );
}

#[test]
fn daily_returns_dated_rows_and_validates_dates_first() {
    let fixture = Fixture::start(|path| {
        assert!(
            path.starts_with("/ExRate/StdExRate_xml.jsp?arr_value="),
            "{path}"
        );
        xml(&chart(
            "기간별 매매기준율",
            &[("99.12.30", "1138"), ("00.01.04", "1128.5")],
        ))
    });
    let (output, value) = run(&mut smbs(
        Some(&fixture),
        &state_dir("daily"),
        Some("0"),
        &[
            "daily",
            "--currency",
            "usd",
            "--from",
            "1999-12-30",
            "--to",
            "2000-01-04",
        ],
    ));
    assert_eq!(output.status.code(), Some(0), "{value}");
    assert_eq!(
        fixture.paths(),
        ["/ExRate/StdExRate_xml.jsp?arr_value=USD_1999-12-30_2000-01-04"]
    );
    let result = &value["result"];
    assert_eq!(value["operation"], "daily");
    assert_eq!(result["basis"], "daily");
    assert_eq!(result["from"], "1999-12-30");
    assert_eq!(result["to"], "2000-01-04");
    let series = &result["series"][0];
    assert_eq!(series["basis"], "daily");
    assert_eq!(series["unit"], 1);
    assert_eq!(
        series["sourceUrl"],
        "http://www.smbs.biz/ExRate/StdExRate_xml.jsp?arr_value=USD_1999-12-30_2000-01-04"
    );
    assert_eq!(
        result["series"][0]["observations"],
        serde_json::json!([
            {"date": "1999-12-30", "rate": 1138.0},
            {"date": "2000-01-04", "rate": 1128.5},
        ])
    );

    for (args, parameter) in [
        (
            &["daily", "--currency", "USD", "--from", "2025-02-30"][..],
            "from",
        ),
        (
            &["daily", "--currency", "USD", "--from", "2025-01"][..],
            "from",
        ),
        (
            &[
                "daily",
                "--currency",
                "USD",
                "--from",
                "1990-01-01",
                "--to",
                "2090-01-01",
            ][..],
            "to",
        ),
    ] {
        let (output, value) = run(&mut smbs(
            Some(&fixture),
            &state_dir("daily"),
            Some("0"),
            args,
        ));
        assert_eq!(output.status.code(), Some(2), "{args:?}: {value}");
        assert_eq!(value["operation"], "daily");
        assert_eq!(value["error"]["parameter"], parameter, "{args:?}");
    }
    assert_eq!(fixture.paths().len(), 1, "invalid dates sent no request");
}

#[test]
fn each_endpoint_has_its_own_body_cap() {
    // A valid chart padded to 2 MiB: over the monthly cap, under the daily cap.
    let padded = |caption: &str, label: &str| {
        let mut body = chart(caption, &[(label, "1.5")]);
        let at = body.len() - b"</chart>\r\n".len();
        body.splice(at..at, std::iter::repeat_n(b' ', 2 << 20));
        xml(&body)
    };
    let monthly = Fixture::start(move |_| padded("월말 매매기준율", "2025.01"));
    let (output, value) = run(&mut smbs(
        Some(&monthly),
        &state_dir("cap"),
        Some("0"),
        &[
            "month-end",
            "--currency",
            "USD",
            "--from",
            "2025-01",
            "--to",
            "2025-01",
        ],
    ));
    assert_eq!(output.status.code(), Some(1), "{value}");
    assert_eq!(value["error"]["code"], "source_changed");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("more than")
    );

    let daily = Fixture::start(move |_| padded("기간별 매매기준율", "25.01.02"));
    let (output, value) = run(&mut smbs(
        Some(&daily),
        &state_dir("cap"),
        Some("0"),
        &[
            "daily",
            "--currency",
            "USD",
            "--from",
            "2025-01-02",
            "--to",
            "2025-01-02",
        ],
    ));
    assert_eq!(output.status.code(), Some(0), "{value}");
    assert_eq!(value["result"]["series"][0]["observations"][0]["rate"], 1.5);
}

fn output_dir(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("smbs-cli-output-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

fn two_currency_fixture() -> Fixture {
    Fixture::start(|path| {
        let rows: &[(&str, &str)] = if path.contains("JPY") {
            &[("2025.02", "900.5")]
        } else {
            &[("2025.01", "1000.5"), ("2025.02", "1001.25")]
        };
        xml(&chart("월말 매매기준율", rows))
    })
}

fn month_end_to(fixture: &Fixture, path: &std::path::Path, extra: &[&str]) -> (Output, Value) {
    let path = path.to_str().unwrap();
    let mut args = vec![
        "month-end",
        "--currency",
        "USD,JPY",
        "--from",
        "2025-01",
        "--to",
        "2025-02",
        "--output",
        path,
    ];
    args.extend_from_slice(extra);
    run(&mut smbs(
        Some(fixture),
        &state_dir("output"),
        Some("0"),
        &args,
    ))
}

#[test]
fn csv_output_is_a_wide_table_reported_in_the_envelope() {
    let fixture = two_currency_fixture();
    let directory = output_dir("csv");
    let path = directory.join("rates.csv");
    let (output, value) = month_end_to(&fixture, &path, &[]);
    assert_eq!(output.status.code(), Some(0), "{value}");
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        bytes,
        "\u{feff}month,USD,JPY (100)\r\n2025-01,1000.5,\r\n2025-02,1001.25,900.5\r\n".as_bytes()
    );
    let artifact = &value["result"]["artifact"];
    assert_eq!(artifact["format"], "csv");
    assert_eq!(artifact["rows"], 2);
    assert_eq!(artifact["bytes"], bytes.len());
    assert_eq!(artifact["sha256"].as_str().unwrap().len(), 64);
    assert!(PathBuf::from(artifact["path"].as_str().unwrap()).is_absolute());
    // The JSON result still carries the series.
    assert_eq!(value["result"]["series"].as_array().unwrap().len(), 2);

    // No-clobber: the second run fails before any request and keeps the file.
    let requests = fixture.paths().len();
    let (output, value) = month_end_to(&fixture, &path, &[]);
    assert_eq!(output.status.code(), Some(2), "{value}");
    assert_eq!(value["error"]["parameter"], "output");
    assert_eq!(fixture.paths().len(), requests);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);

    // --overwrite replaces it and leaves no temporary files behind.
    std::fs::write(&path, "old").unwrap();
    let (output, _) = month_end_to(&fixture, &path, &["--overwrite"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn xlsx_output_holds_numbers_and_dates() {
    let fixture = two_currency_fixture();
    let directory = output_dir("xlsx");
    let path = directory.join("rates.XLSX");
    let (output, value) = month_end_to(&fixture, &path, &[]);
    assert_eq!(output.status.code(), Some(0), "{value}");
    assert_eq!(value["result"]["artifact"]["format"], "xlsx");

    let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let read = |archive: &mut zip::ZipArchive<std::fs::File>, name: &str| {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut archive.by_name(name).unwrap(), &mut text).unwrap();
        text
    };
    let workbook = read(&mut archive, "xl/workbook.xml");
    assert!(workbook.contains("name=\"month-end\""), "{workbook}");
    let sheet = read(&mut archive, "xl/worksheets/sheet1.xml");
    for number in ["1000.5", "1001.25", "900.5"] {
        assert!(
            sheet.contains(&format!("<v>{number}</v>")),
            "{number}: {sheet}"
        );
    }
    // 2025-01-01 and 2025-02-01 as Excel serial dates.
    assert!(
        sheet.contains("<v>45658</v>") && sheet.contains("<v>45689</v>"),
        "{sheet}"
    );
    assert!(sheet.contains("<pane ySplit=\"1\""), "header row is frozen");
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn output_problems_are_invalid_requests_or_output_failures() {
    let fixture = two_currency_fixture();
    let directory = output_dir("problems");
    for path in [
        directory.join("rates.txt"),
        directory.join("missing").join("rates.csv"),
    ] {
        let (output, value) = month_end_to(&fixture, &path, &[]);
        assert_eq!(output.status.code(), Some(2), "{value}");
        assert_eq!(value["error"]["parameter"], "output");
    }
    assert!(fixture.paths().is_empty());

    // A directory in the way is rejected up front, with or without --overwrite.
    let blocked = directory.join("blocked.csv");
    std::fs::create_dir(&blocked).unwrap();
    for extra in [&[][..], &["--overwrite"][..]] {
        let (output, value) = month_end_to(&fixture, &blocked, extra);
        assert_eq!(output.status.code(), Some(2), "{extra:?}: {value}");
        assert_eq!(value["error"]["parameter"], "output");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("is a directory")
        );
    }
    assert!(fixture.paths().is_empty());
    assert!(blocked.is_dir());

    let (output, value) = run(&mut smbs(
        None,
        &state_dir("output"),
        Some("0"),
        &[
            "month-end",
            "--currency",
            "USD",
            "--from",
            "2025-01",
            "--overwrite",
        ],
    ));
    assert_eq!(
        output.status.code(),
        Some(2),
        "--overwrite needs --output: {value}"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn daily_xlsx_uses_day_dates_and_relative_paths_resolve() {
    let fixture = Fixture::start(|_| {
        xml(&chart(
            "기간별 매매기준율",
            &[("25.03.28", "1466.8"), ("25.03.31", "1466.5")],
        ))
    });
    let directory = output_dir("daily-xlsx");
    let (output, value) = run(smbs(
        Some(&fixture),
        &state_dir("output"),
        Some("0"),
        &[
            "daily",
            "--currency",
            "USD",
            "--from",
            "2025-03-28",
            "--to",
            "2025-03-31",
            "--output",
            "march.xlsx",
        ],
    )
    .current_dir(&directory));
    assert_eq!(output.status.code(), Some(0), "{value}");
    let path = directory.join("march.xlsx");
    assert_eq!(
        PathBuf::from(value["result"]["artifact"]["path"].as_str().unwrap()),
        std::path::absolute(&path).unwrap()
    );
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut read = |name: &str| {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut archive.by_name(name).unwrap(), &mut text).unwrap();
        text
    };
    assert!(read("xl/workbook.xml").contains("name=\"daily\""));
    // 2025-03-28 and 2025-03-31 as Excel serial dates, formatted as days.
    let sheet = read("xl/worksheets/sheet1.xml");
    assert!(
        sheet.contains("<v>45744</v>") && sheet.contains("<v>45747</v>"),
        "{sheet}"
    );
    assert!(read("xl/styles.xml").contains("formatCode=\"yyyy-mm-dd\""));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn an_empty_series_still_writes_a_header_only_table() {
    let fixture = Fixture::start(|_| xml(&chart("월평균 매매기준율", &[])));
    let directory = output_dir("empty");
    let path = directory.join("empty.csv");
    let (output, value) = run(&mut smbs(
        Some(&fixture),
        &state_dir("output"),
        Some("0"),
        &[
            "monthly-average",
            "--currency",
            "EUR",
            "--from",
            "2025-01",
            "--to",
            "2025-01",
            "--output",
            path.to_str().unwrap(),
        ],
    ));
    assert_eq!(output.status.code(), Some(0), "{value}");
    assert_eq!(value["result"]["artifact"]["rows"], 0);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        "\u{feff}month,EUR\r\n".as_bytes()
    );
    std::fs::remove_dir_all(directory).unwrap();
}
