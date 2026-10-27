//! What Super is used for, end to end, over every client protocol (HTTP/1.1, HTTP/2, HTTP/3):
//! external services over TLS, the answers of Super's own blocks, and the docs' examples

// Test code: an unexpected value fails the test, loudly
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines
)]

mod harness;

use harness::{service::SERVICE, Harness};
use http::HeaderMap;

#[derive(Clone, Copy, Debug)]
enum Client {
    Http1,
    Http2,
    Http3,
}

/// Every client protocol, HTTP/1.1 first: its HTTPS handshake readies a host's certificate for
/// HTTP/3
const CLIENTS: [Client; 3] = [Client::Http1, Client::Http2, Client::Http3];

/// A response: its status, headers and body
type Answer = (u16, HeaderMap, String);

/// `GET path` from `host`, as `client` sends it (with a header Cloudflare adds,
/// `cf-connecting-ip`)
async fn get(proksi: &Harness, client: Client, host: &str, path: &str) -> Answer {
    let headers = [("cf-connecting-ip", "203.0.113.7")];
    send(proksi, client, "GET", host, path, &headers, b"").await
}

/// `method` `path` from `host`, as `client` sends it, with `headers` and `body`
async fn send(
    proksi: &Harness,
    client: Client,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Answer {
    let sender = match client {
        Client::Http1 => proksi.client(),
        Client::Http2 => proksi.client_h2(),
        Client::Http3 => {
            let answer = proksi.http3_request(method, host, path, headers, body);
            return answer.await.unwrap();
        }
    };
    let url = format!("https://{host}:{}{path}", proksi.https_port);
    let mut request = sender
        .request(method.parse().unwrap(), url)
        .body(body.to_vec());
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await.unwrap();
    let wanted = match client {
        Client::Http1 => http::Version::HTTP_11,
        Client::Http2 | Client::Http3 => http::Version::HTTP_2,
    };
    assert_eq!(response.version(), wanted, "{client:?} {host}{path}");
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    (status, headers, response.text().await.unwrap())
}

/// The request the service got, as it echoed it
fn seen(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or_else(|_| panic!("not the service's answer: {body}"))
}

/// Super in front of `hosts` (HCL), with HTTP/3, its client resolving them: each host `names`
/// has gets the tests' certificate
async fn start(hosts: &str, names: &[&str]) -> Harness {
    let mut hosts = hosts.to_string();
    for name in names {
        let block = format!("host \"{name}\" {{");
        let certificate = "tls {\n    certificate {\n      pem = \"{cert_pem}\"\n      key = \"{cert_key}\"\n    }\n  }";
        hosts = hosts.replace(&block, &format!("{block}\n  {certificate}\n"));
    }
    let mut proksi = Harness::start_with_server(vec![], "http3 = true", &hosts).await;
    for name in names {
        proksi.resolve(name);
    }
    proksi
}

/// An external service behind a CDN (docs, an API), proxied under a host of its own: its
/// `Host`, a path prefix, Cloudflare's headers kept from it and its own response headers kept
/// from clients. This is superproxy.dev's configuration, in front of docs.page.
#[tokio::test(flavor = "multi_thread")]
async fn test_an_external_service_over_tls() {
    let service = format!(
        r#"upstream "127.0.0.1:{{service_port}}" {{
      tls = true
      sni = "{SERVICE}"
      headers {{
        add = {{ "Host" = "{SERVICE}" }}
      }}
    }}"#
    );
    let hosts = format!(
        r#"
host "docs.test" {{
  headers {{
    response {{
      remove = ["x-powered-by"]
    }}
  }}

  rule "no cloudflare headers" {{
    and {{
      path {{
        prefix = "/"
      }}
    }}
    then {{
      headers {{
        request {{
          remove = ["cf-connecting-ip"]
        }}
      }}
    }}
  }}

  location "/" {{
    rewrite {{
      add_prefix = "/owner/repo"
    }}
    {service}
  }}

  location "/assets" {{
    {service}
  }}
}}
"#
    );
    let mut proksi = start(&hosts, &["docs.test"]).await;

    for client in CLIENTS {
        for (path, sent) in [
            ("/guide?page=2", "/owner/repo/guide?page=2"),
            ("/assets/app.js", "/assets/app.js"),
        ] {
            let (status, headers, body) = get(&proksi, client, "docs.test", path).await;
            assert_eq!(status, 200, "{client:?} {path}: {body}\n{}", proksi.logs());
            let seen = seen(&body);
            assert_eq!(seen["path"], sent, "{client:?}");
            assert_eq!(seen["host"], SERVICE, "{client:?}");
            assert_eq!(seen["sni"], SERVICE, "{client:?}");
            assert_eq!(seen["version"], "HTTP/2.0", "{client:?}");
            assert!(
                seen["headers"].get("cf-connecting-ip").is_none(),
                "{client:?}"
            );
            assert!(!headers.contains_key("x-powered-by"), "{client:?}");
        }
    }
    proksi.assert_healthy();
}

/// Upstreams' certificates are checked with `verify`: signed by an authority the system trusts
/// (the tests trust the service's), for `sni`
#[tokio::test(flavor = "multi_thread")]
async fn test_upstream_certificates_are_checked() {
    let upstream = |name: &str, verify: &str, tls: &str| {
        format!(
            r#"
host "{name}" {{
  location "/" {{
    upstream "127.0.0.1:{{service_port}}" {{
      {tls}
      {verify}
      sni = "{SERVICE}"
      headers {{
        add = {{ "Host" = "{SERVICE}" }}
      }}
    }}
  }}
}}
"#
        )
    };
    let wrong_name = r#"
host "wrong-name.test" {
  location "/" {
    upstream "127.0.0.1:{service_port}" {
      tls = true
      verify = true
      sni = "other.test"
      headers {
        add = { "Host" = "other.test" }
      }
    }
  }
}
"#;
    let hosts = [
        upstream("verified.test", "verify = true", "tls = true"),
        upstream("unchecked.test", "", "tls = true"),
        // Without `tls`, HTTP on a port other than 443
        upstream("plain.test", "", ""),
        wrong_name.to_string(),
    ]
    .concat();
    let names = [
        "verified.test",
        "unchecked.test",
        "plain.test",
        "wrong-name.test",
    ];
    let proksi = start(&hosts, &names).await;

    for client in CLIENTS {
        let (status, _, body) = get(&proksi, client, "verified.test", "/").await;
        assert_eq!(status, 200, "{client:?}: {body}\n{}", proksi.logs());
        assert_eq!(seen(&body)["sni"], SERVICE);
        // Its certificate isn't for `other.test`: no connection
        let (status, _, _) = get(&proksi, client, "wrong-name.test", "/").await;
        assert_eq!(status, 502, "{client:?}");
        // Unchecked, it's connected to
        let (status, _, _) = get(&proksi, client, "unchecked.test", "/").await;
        assert_eq!(status, 200, "{client:?}");
        // HTTP to a TLS port: no answer
        let (status, _, _) = get(&proksi, client, "plain.test", "/").await;
        assert_eq!(status, 502, "{client:?}");
    }
}

/// What Super answers itself, without its upstream (redirects, set answers, refusals, files):
/// over every protocol, with the host's response headers (the location's blocks' answers go
/// through them, as upstreams' responses do)
#[tokio::test(flavor = "multi_thread")]
async fn test_answers_of_super_itself() {
    let files = tempfile::tempdir().unwrap();
    std::fs::write(files.path().join("hello.txt"), "hi").unwrap();
    let hosts = format!(
        r#"
host "own.test" {{
  headers {{
    response {{
      add = {{ "x-host" = "own" }}
    }}
  }}

  location "/redirect" {{
    redirect {{
      to     = "https://example.com{{uri}}"
      status = 301
    }}
  }}

  location "/respond" {{
    respond {{
      status       = 503
      body         = "Back soon"
      content_type = "text/plain"
    }}
  }}

  location "/auth" {{
    upstream "127.0.0.1:{{http_upstream_port}}" {{}}
    basic_auth {{
      user = "user"
      pass = "pass"
    }}
  }}

  location "/limited" {{
    upstream "127.0.0.1:{{http_upstream_port}}" {{}}
    rate_limit {{
      requests = 1
      per      = "1h"
    }}
  }}

  location "/blocked" {{
    upstream "127.0.0.1:{{http_upstream_port}}" {{}}
    ip_filter {{
      deny = ["127.0.0.1"]
    }}
  }}

  location "/files" {{
    static_files {{
      directory = "{files}"
    }}
  }}

  location "/cors" {{
    upstream "127.0.0.1:{{http_upstream_port}}" {{}}
    cors {{
      origins = ["https://app.test"]
    }}
  }}

  location "/upload" {{
    upstream "127.0.0.1:{{http_upstream_port}}" {{}}
    body_limit {{
      max = "4B"
    }}
  }}
}}
"#,
        files = files.path().display()
    );
    let mut proksi = start(&hosts, &["own.test"]).await;
    let preflight = [
        ("origin", "https://app.test"),
        ("access-control-request-method", "PUT"),
    ];
    let credentials = [("authorization", "Basic dXNlcjpwYXNz")];

    for client in CLIENTS {
        let ask = |method, path, headers, body| {
            send(&proksi, client, method, "own.test", path, headers, body)
        };
        let (status, headers, _) = ask("GET", "/redirect/a?b=1", &[], b"").await;
        assert_eq!(status, 301, "{client:?}");
        assert_eq!(headers["location"], "https://example.com/redirect/a?b=1");
        assert_eq!(headers["x-host"], "own", "{client:?} redirect");

        let (status, headers, body) = ask("GET", "/respond", &[], b"").await;
        assert_eq!((status, body.as_str()), (503, "Back soon"), "{client:?}");
        assert_eq!(headers["x-host"], "own", "{client:?} respond");

        let (status, headers, _) = ask("GET", "/auth", &[], b"").await;
        assert_eq!(status, 401, "{client:?}");
        assert!(headers.contains_key("www-authenticate"), "{client:?}");
        assert_eq!(headers["x-host"], "own", "{client:?} basic_auth");
        let (status, _, _) = ask("GET", "/auth", &credentials, b"").await;
        assert_eq!(status, 200, "{client:?} signed in");

        // One request an hour, from one address: the second is refused, whichever came first
        let _ = ask("GET", "/limited", &[], b"").await;
        let (status, headers, _) = ask("GET", "/limited", &[], b"").await;
        assert_eq!(status, 429, "{client:?}");
        assert_eq!(headers["x-host"], "own", "{client:?} rate_limit");

        let (status, headers, _) = ask("GET", "/blocked", &[], b"").await;
        assert_eq!(status, 403, "{client:?}");
        assert_eq!(headers["x-host"], "own", "{client:?} ip_filter");

        let (status, headers, body) = ask("GET", "/files/hello.txt", &[], b"").await;
        assert_eq!((status, body.as_str()), (200, "hi"), "{client:?}");
        assert_eq!(headers["x-host"], "own", "{client:?} static_files");

        let (status, headers, _) = ask("OPTIONS", "/cors", &preflight, b"").await;
        assert_eq!(status, 204, "{client:?}");
        assert_eq!(headers["access-control-allow-origin"], "https://app.test");
        assert_eq!(headers["x-host"], "own", "{client:?} cors");

        let (status, headers, _) = ask("POST", "/upload", &[], b"far too long").await;
        assert_eq!(status, 413, "{client:?}");
        assert_eq!(headers["x-host"], "own", "{client:?} body_limit");
    }
    proksi.assert_healthy();
}
