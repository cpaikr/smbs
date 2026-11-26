//! Cached update notices on rate commands, and `version --check`, against
//! loopback origins for both smbs.biz and GitHub releases.
#![cfg(feature = "fixture-origin")]

mod common;

use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use common::{Fixture, response};
use serde_json::Value;

const MANIFEST: &str = "/cpaikr/smbs/releases/latest/download/release-manifest.json";

fn smbs_biz() -> Fixture {
    Fixture::start(|_| {
        let text = "\r\n<?xml version=\"1.0\" encoding=\"EUC-KR\"?>\r\n<chart caption='월말 매매기준율'>\r\n<set label='2025.01' value='1000.5' />\r\n</chart>\r\n";
        let body = encoding_rs::EUC_KR.encode(text).0.into_owned();
        response("200 OK", "text/xml; charset=EUC-KR", "", &body)
    })
}

fn target_key() -> &'static str {
    if cfg!(windows) {
        "windows-x64-msvc"
    } else if cfg!(target_os = "macos") {
        "darwin-arm64"
    } else if cfg!(target_arch = "aarch64") {
        "linux-arm64-gnu"
    } else {
        "linux-x64-gnu"
    }
}

/// A release origin whose latest release is `version`, with or without a
/// binary for this platform.
fn releases_with(version: &'static str, installable: bool) -> Fixture {
    let extension = if cfg!(windows) { "zip" } else { "tar.gz" };
    let targets = if installable {
        format!(
            r#"{{"{key}":{{"asset":"smbs-{version}-{key}.{extension}","sha256":"{sha}"}}}}"#,
            key = target_key(),
            sha = "a".repeat(64)
        )
    } else {
        "{}".to_owned()
    };
    let manifest = format!(
        r#"{{"schemaVersion":1,"version":"{version}","tag":"v{version}","targets":{targets}}}"#
    );
    Fixture::start(move |path| {
        if path == MANIFEST {
            response("200 OK", "application/json", "", manifest.as_bytes())
        } else {
            response("404 Not Found", "text/plain", "", b"")
        }
    })
}

fn releases(version: &'static str) -> Fixture {
    releases_with(version, true)
}

fn cache_dir(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("smbs-notices-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    directory
}

fn run(
    release_origin: &str,
    cache: &PathBuf,
    running: &str,
    extra_env: &[(&str, &str)],
    args: &[&str],
) -> Value {
    let source = smbs_biz();
    let mut command = Command::new(env!("CARGO_BIN_EXE_smbs"));
    command
        .args(args)
        .env("SMBS_FIXTURE_ORIGIN", &source.origin)
        .env("SMBS_FIXTURE_RELEASE_ORIGIN", release_origin)
        .env("SMBS_FIXTURE_RUNNING_VERSION", running)
        .env("SMBS_CACHE_DIR", cache)
        .env("SMBS_REQUEST_INTERVAL_MS", "0")
        .env_remove("SMBS_NO_UPDATE_CHECK");
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(0));
    serde_json::from_slice(&output.stdout).unwrap()
}

const RATES: &[&str] = &[
    "month-end",
    "--currency",
    "USD",
    "--from",
    "2025-01",
    "--to",
    "2025-01",
];

fn manifest_requests(fixture: &Fixture) -> usize {
    fixture
        .paths()
        .iter()
        .filter(|path| *path == MANIFEST)
        .count()
}

#[test]
fn a_newer_release_adds_one_cached_advisory() {
    let github = releases("9.9.9");
    let cache = cache_dir("newer");

    let first = run(&github.origin, &cache, "0.0.1", &[], RATES);
    let advisories = first["advisories"].as_array().unwrap();
    assert_eq!(advisories.len(), 1, "{first}");
    assert_eq!(advisories[0]["code"], "update_available");
    assert!(advisories[0]["message"].as_str().unwrap().contains("9.9.9"));
    assert_eq!(
        advisories[0]["releaseUrl"],
        "https://github.com/cpaikr/smbs/releases/tag/v9.9.9"
    );
    assert_eq!(
        first["result"]["series"][0]["observations"][0]["rate"],
        1000.5
    );
    assert_eq!(manifest_requests(&github), 1);

    // Fresh evidence is reused, and compared with the running version each
    // time: once "upgraded", the notice disappears without a refresh.
    let second = run(&github.origin, &cache, "0.0.1", &[], RATES);
    assert_eq!(second["advisories"][0]["code"], "update_available");
    let current = run(&github.origin, &cache, "9.9.9", &[], RATES);
    assert!(current.get("advisories").is_none(), "{current}");
    assert_eq!(manifest_requests(&github), 1);

    // An explicit check always refreshes.
    let check = run(
        &github.origin,
        &cache,
        "0.0.1",
        &[],
        &["version", "--check"],
    );
    let release_check = &check["result"]["releaseCheck"];
    assert_eq!(release_check["comparison"], "newer");
    assert_eq!(release_check["freshness"], "fresh");
    assert_eq!(release_check["installable"], true);
    assert_eq!(manifest_requests(&github), 2);
    let _ = std::fs::remove_dir_all(cache);
}

#[test]
fn opting_out_skips_the_check_entirely() {
    let github = releases("9.9.9");
    let cache = cache_dir("opt-out");
    let value = run(
        &github.origin,
        &cache,
        "0.0.1",
        &[("SMBS_NO_UPDATE_CHECK", "1")],
        RATES,
    );
    assert!(value.get("advisories").is_none());
    assert_eq!(github.paths(), Vec::<String>::new());
    assert!(!cache.exists(), "no cache work when opted out");
}

#[test]
fn a_failed_refresh_waits_out_its_cooldown() {
    let unavailable =
        Fixture::start(|_| response("503 Service Unavailable", "text/plain", "", b""));
    let offline = unavailable.origin.clone();
    let cache = cache_dir("cooldown");

    let first = run(&offline, &cache, "0.0.1", &[], RATES);
    assert_eq!(first["ok"], true, "a failed check never fails the command");
    assert_eq!(first["advisories"][0]["code"], "update_check_unavailable");

    // Within the cooldown the next run makes no request, even to a working origin.
    let github = releases("9.9.9");
    let second = run(&github.origin, &cache, "0.0.1", &[], RATES);
    assert_eq!(second["advisories"][0]["code"], "update_check_unavailable");
    assert_eq!(github.paths(), Vec::<String>::new());

    // `version --check` ignores the cooldown and reports what happened.
    let check = run(&offline, &cache, "0.0.1", &[], &["version", "--check"]);
    let release_check = &check["result"]["releaseCheck"];
    assert_eq!(release_check["comparison"], "unavailable");
    assert!(release_check["problem"].as_str().is_some());
    let _ = std::fs::remove_dir_all(cache);
}

#[test]
fn offline_commands_and_failures_never_check() {
    let github = releases("9.9.9");
    let cache = cache_dir("offline-commands");
    for args in [&["currencies"][..], &["version"][..], &["--version"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_smbs"))
            .args(args)
            .env("SMBS_FIXTURE_RELEASE_ORIGIN", &github.origin)
            .env("SMBS_CACHE_DIR", &cache)
            .env_remove("SMBS_NO_UPDATE_CHECK")
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}");
    }
    let invalid = Command::new(env!("CARGO_BIN_EXE_smbs"))
        .args(["month-end", "--currency", "XXX", "--from", "2025-01"])
        .env("SMBS_FIXTURE_RELEASE_ORIGIN", &github.origin)
        .env("SMBS_CACHE_DIR", &cache)
        .env_remove("SMBS_NO_UPDATE_CHECK")
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    assert_eq!(github.paths(), Vec::<String>::new());
}

#[test]
fn a_release_without_this_platform_is_not_offered_as_an_upgrade() {
    let github = releases_with("9.9.9", false);
    let cache = cache_dir("incomplete");
    let value = run(&github.origin, &cache, "0.0.1", &[], RATES);
    let advisory = &value["advisories"][0];
    assert_eq!(
        advisory["code"], "update_incomplete_distribution",
        "{value}"
    );
    assert!(
        !advisory["message"]
            .as_str()
            .unwrap()
            .contains("smbs upgrade")
    );
    let _ = std::fs::remove_dir_all(cache);
}

#[test]
fn a_slow_release_origin_cannot_hold_up_a_command() {
    let slow = Fixture::start(|_| {
        std::thread::sleep(Duration::from_secs(5));
        response("503 Service Unavailable", "text/plain", "", b"")
    });
    let cache = cache_dir("slow");
    let started = Instant::now();
    let value = run(&slow.origin, &cache, "0.0.1", &[], RATES);
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(
        value["result"]["series"][0]["observations"][0]["rate"],
        1000.5
    );
    let _ = std::fs::remove_dir_all(cache);
}

#[test]
fn persistent_problems_are_reported_at_once() {
    let future = Fixture::start(|path| {
        if path == MANIFEST {
            let manifest = r#"{"schemaVersion":2,"version":"9.9.9","tag":"v9.9.9","targets":{}}"#;
            response("200 OK", "application/json", "", manifest.as_bytes())
        } else {
            response("404 Not Found", "text/plain", "", b"")
        }
    });
    let cache = cache_dir("problem");
    let value = run(&future.origin, &cache, "0.0.1", &[], RATES);
    let advisory = &value["advisories"][0];
    assert_eq!(advisory["code"], "update_check_problem", "{value}");
    assert!(advisory["message"].as_str().unwrap().contains("schema 2"));

    // A relative cache directory is a problem too, not a silent opt-out.
    let github = releases("9.9.9");
    let value = run(
        &github.origin,
        &PathBuf::from("relative-cache"),
        "0.0.1",
        &[],
        RATES,
    );
    assert_eq!(
        value["advisories"][0]["code"], "update_check_problem",
        "{value}"
    );
    assert!(
        value["advisories"][0]["message"]
            .as_str()
            .unwrap()
            .contains("SMBS_CACHE_DIR")
    );
    assert_eq!(github.paths(), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(cache);
}
