//! What HTTP's RFCs require of a proxy, row by row: each row is a requirement of a section
//! (`MUST` or `SHOULD`), checked against Super in front of an upstream keeping what it gets.
//!
//! The rows are in `spec/rfc<number>.rs`. The ones known to fail are listed, with why, in
//! `conformance/expected/super.txt`: a row failing that isn't listed fails the test, and so does
//! one listed that passes. The results go to `conformance/results/super.json`, for the README.

// Test code: an unexpected value fails the test, loudly
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines
)]

mod harness;
mod spec {
    pub mod rfc9110;
    pub mod rfc9112;
    pub mod rfc9113;
    pub mod rfc9114;
    pub mod upstream;
}

use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use futures_util::future::BoxFuture;
use harness::{Answer, Client, Harness};
use spec::upstream::Upstream;

/// Whether a requirement is a `MUST` or a `SHOULD`
#[derive(Clone, Copy, Debug)]
pub enum Level {
    Must,
    Should,
}

/// A requirement's check: `Err` says what was wrong
pub type Check<'a> = BoxFuture<'a, Result<(), String>>;

/// A requirement of an RFC's section on a proxy
pub struct Requirement {
    pub rfc: u16,
    pub section: &'static str,
    pub level: Level,
    /// What's required, in short
    pub says: &'static str,
    pub check: for<'a> fn(&'a Ctx) -> Check<'a>,
}

impl Requirement {
    /// How it's listed: `RFC 9110 §7.6.1 MUST: ...`
    fn id(&self) -> String {
        let level = match self.level {
            Level::Must => "MUST",
            Level::Should => "SHOULD",
        };
        format!("RFC {} §{} {level}: {}", self.rfc, self.section, self.says)
    }
}

/// The host served over plain HTTP (raw HTTP/1.1 requests), and the one over HTTPS (HTTP/1.1,
/// HTTP/2 and HTTP/3 clients), both in front of the upstream
pub const PLAIN: &str = "plain.spec.test";
pub const TLS: &str = "tls.spec.test";

/// What a requirement is checked against
pub struct Ctx {
    pub proksi: Harness,
    pub upstream: Upstream,
}

impl Ctx {
    /// `request`, sent as it is on plain HTTP: the response, until Super closes the connection
    /// (empty when it closes without one)
    pub async fn raw(&self, request: &str) -> String {
        let port = self.proksi.http_port;
        let response = self.proksi.raw_http_on(port, request.as_bytes()).await;
        response.unwrap_or_default()
    }

    /// A request to the HTTPS host, as `client` sends it
    pub async fn send(
        &self,
        client: Client,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
    ) -> Answer {
        self.proksi
            .send(client, method, TLS, path, headers, b"")
            .await
    }
}

/// The status of a raw response (`0` without one)
pub fn status(response: &str) -> u16 {
    response
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse().ok())
        .unwrap_or(0)
}

/// `Err(why)` unless `holds`
pub fn ensure(holds: bool, why: impl FnOnce() -> String) -> Result<(), String> {
    if holds {
        Ok(())
    } else {
        Err(why())
    }
}

fn conformance() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
}

/// The rows known to fail, and why (`expected/super.txt`: the row, ` # `, why)
fn expected() -> BTreeMap<String, String> {
    let text =
        std::fs::read_to_string(conformance().join("expected/super.txt")).unwrap_or_default();
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (row, why) = line.split_once(" # ").unwrap_or((line, ""));
            (row.trim().to_string(), why.trim().to_string())
        })
        .collect()
}

async fn start() -> Ctx {
    let upstream = Upstream::start().await;
    let location = format!(
        "  location \"/\" {{\n    upstream \"127.0.0.1:{}\" {{}}\n  }}\n",
        upstream.port
    );
    let hosts = format!(
        "host \"{PLAIN}\" {{\n  tls = false\n{location}}}\n\n\
         host \"{TLS}\" {{\n  tls {{\n    certificate {{\n      pem = \"{{cert_pem}}\"\n      \
         key = \"{{cert_key}}\"\n    }}\n  }}\n{location}}}\n"
    );
    let mut proksi = Harness::start_with_server(vec![], "http3 = true", &hosts).await;
    proksi.resolve(TLS);
    // Its HTTPS handshake readies the host's certificate for HTTP/3
    proksi
        .client()
        .get(format!("https://{TLS}:{}/", proksi.https_port))
        .send()
        .await
        .unwrap();
    Ctx { proksi, upstream }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_what_the_rfcs_require_of_a_proxy() {
    let ctx = start().await;
    let rows: Vec<Requirement> = [
        spec::rfc9110::rows(),
        spec::rfc9112::rows(),
        spec::rfc9113::rows(),
        spec::rfc9114::rows(),
    ]
    .into_iter()
    .flatten()
    .collect();
    let known = expected();

    let mut passed = vec![];
    let mut failed = BTreeMap::new();
    for row in &rows {
        let checked = tokio::time::timeout(Duration::from_secs(20), (row.check)(&ctx)).await;
        match checked.unwrap_or_else(|_| Err("timed out".to_string())) {
            Ok(()) => passed.push(row.id()),
            Err(why) => {
                failed.insert(row.id(), why);
            }
        }
    }

    let results = serde_json::json!({
        "suite": "super",
        "version": env!("CARGO_PKG_VERSION"),
        "passed": passed.len(),
        "failed": failed.len(),
        "skipped": 0,
        "known": failed.keys().filter_map(|row| Some((row.clone(), known.get(row)?.clone()))).collect::<BTreeMap<_, _>>(),
    });
    let path = conformance().join("results");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("super.json"), format!("{results:#}\n")).unwrap();

    let unexpected: Vec<String> = failed
        .iter()
        .filter(|(row, _)| !known.contains_key(*row))
        .map(|(row, why)| format!("failing, and not in expected/super.txt: {row}\n    {why}"))
        .collect();
    let fixed: Vec<String> = known
        .keys()
        .filter(|row| passed.contains(row))
        .map(|row| format!("passing, but in expected/super.txt (remove it): {row}"))
        .collect();
    let problems = [unexpected, fixed].concat();
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}
