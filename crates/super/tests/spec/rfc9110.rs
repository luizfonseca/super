//! RFC 9110, HTTP semantics: what an intermediary forwards (its connection's fields, `Via`,
//! `Max-Forwards`), responses without a body, and `Expect: 100-continue`

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{
    ensure,
    harness::{Client, CLIENTS},
    status, Check, Ctx,
    Level::{Must, Should},
    Requirement, PLAIN,
};

pub fn rows() -> Vec<Requirement> {
    vec![
        Requirement {
            rfc: 9110,
            section: "7.6.1",
            level: Must,
            says: "Connection, and the fields it lists, aren't forwarded to the upstream",
            check: connection_fields_to_upstream,
        },
        Requirement {
            rfc: 9110,
            section: "7.6.1",
            level: Should,
            says: "Keep-Alive, Proxy-Connection, TE (but trailers) and Upgrade aren't forwarded to the upstream",
            check: hop_by_hop_to_upstream,
        },
        Requirement {
            rfc: 9110,
            section: "7.6.1",
            level: Must,
            says: "Connection, the fields it lists, Keep-Alive and Proxy-Connection from the upstream don't reach clients (HTTP/1.1, HTTP/2, HTTP/3)",
            check: connection_fields_to_client,
        },
        Requirement {
            rfc: 9110,
            section: "7.6.2",
            level: Must,
            says: "an OPTIONS with Max-Forwards: 0 is answered, not forwarded",
            check: max_forwards_zero,
        },
        Requirement {
            rfc: 9110,
            section: "7.6.2",
            level: Must,
            says: "Max-Forwards is decremented in an OPTIONS forwarded",
            check: max_forwards_decremented,
        },
        Requirement {
            rfc: 9110,
            section: "7.6.3",
            level: Must,
            says: "Via is added to the requests forwarded (a gateway MAY add it to responses)",
            check: via,
        },
        Requirement {
            rfc: 9110,
            section: "9.3.2",
            level: Must,
            says: "the response to HEAD has no body (HTTP/1.1, HTTP/2, HTTP/3)",
            check: head_without_body,
        },
        Requirement {
            rfc: 9110,
            section: "8.6",
            level: Must,
            says: "a 204 has no body nor Content-Length (HTTP/1.1, HTTP/2, HTTP/3)",
            check: no_content,
        },
        Requirement {
            rfc: 9110,
            section: "15.4.5",
            level: Must,
            says: "a 304 has no body (HTTP/1.1, HTTP/2, HTTP/3)",
            check: not_modified,
        },
        Requirement {
            rfc: 9110,
            section: "10.1.1",
            level: Must,
            says: "a request with Expect: 100-continue gets a 100 Continue, or its final status, before its body is sent",
            check: expect_continue,
        },
    ]
}

fn connection_fields_to_upstream(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        let request = format!(
            "GET /connection-fields HTTP/1.1\r\nHost: {PLAIN}\r\nConnection: x-secret, close\r\n\
             X-Secret: 1\r\n\r\n"
        );
        ctx.raw(&request).await;
        let forwarded = ctx
            .upstream
            .request("/connection-fields")
            .await
            .ok_or("not forwarded")?;
        let kept: Vec<&str> = ["connection", "x-secret"]
            .into_iter()
            .filter(|name| forwarded.has(name))
            .collect();
        ensure(kept.is_empty(), || format!("forwarded {kept:?}"))
    })
}

fn hop_by_hop_to_upstream(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        let request = format!(
            "GET /hop-by-hop HTTP/1.1\r\nHost: {PLAIN}\r\nKeep-Alive: timeout=5\r\n\
             Proxy-Connection: keep-alive\r\nTE: gzip\r\nUpgrade: h2c\r\nConnection: close\r\n\r\n"
        );
        ctx.raw(&request).await;
        let forwarded = ctx
            .upstream
            .request("/hop-by-hop")
            .await
            .ok_or("not forwarded")?;
        let kept: Vec<&str> = ["keep-alive", "proxy-connection", "te", "upgrade"]
            .into_iter()
            .filter(|name| forwarded.has(name))
            .collect();
        ensure(kept.is_empty(), || format!("forwarded {kept:?}"))
    })
}

/// Each client protocol's response to `method` `path`, checked by `holds` (what's wrong)
async fn every_client<F>(ctx: &Ctx, method: &str, path: &str, holds: F) -> Result<(), String>
where
    F: Fn(Client, &crate::harness::Answer) -> Option<String>,
{
    let mut wrong = vec![];
    for client in CLIENTS {
        let answer = ctx.send(client, method, path, &[]).await;
        wrong.extend(holds(client, &answer));
    }
    ensure(wrong.is_empty(), || wrong.join("; "))
}

fn connection_fields_to_client(ctx: &Ctx) -> Check<'_> {
    Box::pin(every_client(
        ctx,
        "GET",
        "/connection-headers",
        |client, (_, headers, _)| {
            let kept: Vec<&str> = ["x-upstream-private", "keep-alive", "proxy-connection"]
                .into_iter()
                .filter(|name| headers.contains_key(*name))
                .collect();
            (!kept.is_empty()).then(|| format!("{client:?} got {kept:?}"))
        },
    ))
}

fn max_forwards_zero(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        let request = format!(
            "OPTIONS /max-forwards-zero HTTP/1.1\r\nHost: {PLAIN}\r\nMax-Forwards: 0\r\n\
             Connection: close\r\n\r\n"
        );
        let response = ctx.raw(&request).await;
        ensure(status(&response) != 0, || "no answer".into())?;
        let forwarded = ctx.upstream.request("/max-forwards-zero").await;
        ensure(forwarded.is_none(), || "forwarded".into())
    })
}

fn max_forwards_decremented(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        let request = format!(
            "OPTIONS /max-forwards HTTP/1.1\r\nHost: {PLAIN}\r\nMax-Forwards: 5\r\n\
             Connection: close\r\n\r\n"
        );
        ctx.raw(&request).await;
        let forwarded = ctx
            .upstream
            .request("/max-forwards")
            .await
            .ok_or("not forwarded")?;
        let value = forwarded.all("max-forwards");
        ensure(value == ["4"], || format!("forwarded with {value:?}"))
    })
}

fn via(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        let request = format!("GET /via HTTP/1.1\r\nHost: {PLAIN}\r\nConnection: close\r\n\r\n");
        ctx.raw(&request).await;
        let forwarded = ctx.upstream.request("/via").await.ok_or("not forwarded")?;
        let via = forwarded.all("via");
        ensure(via == ["1.1 super"], || format!("forwarded with {via:?}"))
    })
}

fn head_without_body(ctx: &Ctx) -> Check<'_> {
    Box::pin(every_client(
        ctx,
        "HEAD",
        "/head",
        |client, (status, _, body)| {
            (*status != 200 || !body.is_empty()).then(|| format!("{client:?}: {status} {body:?}"))
        },
    ))
}

fn no_content(ctx: &Ctx) -> Check<'_> {
    Box::pin(every_client(
        ctx,
        "GET",
        "/no-content",
        |client, (status, headers, body)| {
            let length = headers.contains_key("content-length");
            (*status != 204 || !body.is_empty() || length)
                .then(|| format!("{client:?}: {status}, body {body:?}, Content-Length: {length}"))
        },
    ))
}

fn not_modified(ctx: &Ctx) -> Check<'_> {
    Box::pin(every_client(
        ctx,
        "GET",
        "/not-modified",
        |client, (status, _, body)| {
            (*status != 304 || !body.is_empty()).then(|| format!("{client:?}: {status} {body:?}"))
        },
    ))
}

fn expect_continue(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        let address = ("127.0.0.1", ctx.proksi.http_port);
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .map_err(|e| e.to_string())?;
        let head = format!(
            "POST /expect HTTP/1.1\r\nHost: {PLAIN}\r\nExpect: 100-continue\r\nContent-Length: 5\r\n\
             Connection: close\r\n\r\n"
        );
        stream
            .write_all(head.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        // Without its body, a `100` (or a final status) must come
        let mut first = [0; 12];
        let read = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut first)).await;
        let answered = matches!(read, Ok(Ok(read)) if read > 0);
        ensure(answered, || "nothing came before the body".into())
    })
}
