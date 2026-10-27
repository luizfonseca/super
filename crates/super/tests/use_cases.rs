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

/// `GET path` from `host`, as `client` sends it (HTTP/1.1 and HTTP/2 with a header Cloudflare
/// adds, `cf-connecting-ip`)
async fn get(proksi: &Harness, client: Client, host: &str, path: &str) -> Answer {
    let sender = match client {
        Client::Http1 => proksi.client(),
        Client::Http2 => proksi.client_h2(),
        Client::Http3 => return proksi.http3_get(host, path).await.unwrap(),
    };
    let response = sender
        .get(format!("https://{host}:{}{path}", proksi.https_port))
        .header("cf-connecting-ip", "203.0.113.7")
        .send()
        .await
        .unwrap();
    let version = response.version();
    let wanted = match client {
        Client::Http1 => http::Version::HTTP_11,
        Client::Http2 | Client::Http3 => http::Version::HTTP_2,
    };
    assert_eq!(version, wanted, "{client:?} {host}{path}");
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
    let mut proksi = start(&hosts, &names).await;

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
