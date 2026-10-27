//! RFC 9112, HTTP/1.1: framing a request (where request smuggling comes from), its header
//! fields and its target, as a proxy reads them from clients

use crate::{ensure, status, Check, Ctx, Level::Must, Requirement, PLAIN};

pub fn rows() -> Vec<Requirement> {
    vec![
        Requirement {
            rfc: 9112,
            section: "6.3",
            level: Must,
            says: "a request with Transfer-Encoding and Content-Length is read by its chunks, or refused: what it smuggles never reaches the upstream",
            check: transfer_encoding_and_content_length,
        },
        Requirement {
            rfc: 9112,
            section: "6.1",
            level: Must,
            says: "a request whose Transfer-Encoding doesn't end with chunked gets a 400",
            check: transfer_encoding_not_chunked,
        },
        Requirement {
            rfc: 9112,
            section: "6.3",
            level: Must,
            says: "a request with different Content-Length values gets a 400",
            check: different_content_lengths,
        },
        Requirement {
            rfc: 9112,
            section: "6.3",
            level: Must,
            says: "a request with an invalid Content-Length gets a 400",
            check: invalid_content_length,
        },
        Requirement {
            rfc: 9112,
            section: "5.1",
            level: Must,
            says: "a request with whitespace between a field's name and its colon gets a 400",
            check: whitespace_before_colon,
        },
        Requirement {
            rfc: 9112,
            section: "5.2",
            level: Must,
            says: "a folded field line (obs-fold) is refused, or unfolded before the request goes on",
            check: folded_field,
        },
        Requirement {
            rfc: 9112,
            section: "2.2",
            level: Must,
            says: "a request with a bare CR gets a 400",
            check: bare_cr,
        },
        Requirement {
            rfc: 9112,
            section: "3.2",
            level: Must,
            says: "a request without Host gets a 400",
            check: no_host,
        },
        Requirement {
            rfc: 9112,
            section: "3.2",
            level: Must,
            says: "a request with two Host fields gets a 400",
            check: two_hosts,
        },
        Requirement {
            rfc: 9112,
            section: "3.2.2",
            level: Must,
            says: "an absolute-form target's host is used, not Host's",
            check: absolute_form,
        },
        Requirement {
            rfc: 9112,
            section: "9.6",
            level: Must,
            says: "the connection is closed after the response to a request with Connection: close",
            check: connection_close,
        },
    ]
}

/// A request refused (`400`), that never reached the upstream (by its `path`)
async fn refused(ctx: &Ctx, path: &str, request: &str) -> Result<(), String> {
    let response = ctx.raw(request).await;
    ensure(status(&response) == 400, || {
        format!("got {:?}", head(&response))
    })?;
    let forwarded = ctx.upstream.request(path).await;
    ensure(forwarded.is_none(), || format!("forwarded: {forwarded:?}"))
}

/// A response's status line
fn head(response: &str) -> &str {
    response.lines().next().unwrap_or_default()
}

fn transfer_encoding_and_content_length(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        let request = format!(
            "POST /te-and-cl HTTP/1.1\r\nHost: {PLAIN}\r\nTransfer-Encoding: chunked\r\n\
             Content-Length: 4\r\nConnection: close\r\n\r\n0\r\n\r\n\
             GET /smuggled HTTP/1.1\r\nHost: {PLAIN}\r\n\r\n"
        );
        ctx.raw(&request).await;
        let smuggled = ctx.upstream.request("/smuggled").await;
        ensure(smuggled.is_none(), || {
            "the smuggled request reached the upstream".into()
        })?;
        let Some(forwarded) = ctx.upstream.requests("/te-and-cl").pop() else {
            return Ok(());
        };
        ensure(
            !(forwarded.has("transfer-encoding") && forwarded.has("content-length")),
            || "forwarded with both".into(),
        )
    })
}

fn transfer_encoding_not_chunked(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        let request = format!(
            "POST /te-gzip HTTP/1.1\r\nHost: {PLAIN}\r\nTransfer-Encoding: gzip\r\n\
             Connection: close\r\n\r\nabcd"
        );
        refused(ctx, "/te-gzip", &request).await
    })
}

fn different_content_lengths(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        let request = format!(
            "POST /two-lengths HTTP/1.1\r\nHost: {PLAIN}\r\nContent-Length: 3\r\n\
             Content-Length: 5\r\nConnection: close\r\n\r\nabcde"
        );
        refused(ctx, "/two-lengths", &request).await
    })
}

fn invalid_content_length(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        let request = format!(
            "POST /invalid-length HTTP/1.1\r\nHost: {PLAIN}\r\nContent-Length: 4x\r\n\
             Connection: close\r\n\r\nabcd"
        );
        refused(ctx, "/invalid-length", &request).await
    })
}

fn whitespace_before_colon(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        let request = format!(
            "GET /space-colon HTTP/1.1\r\nHost: {PLAIN}\r\nX-Spaced : 1\r\nConnection: close\r\n\r\n"
        );
        refused(ctx, "/space-colon", &request).await
    })
}

fn folded_field(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        let request = format!(
            "GET /folded HTTP/1.1\r\nHost: {PLAIN}\r\nX-Folded: a\r\n b\r\nConnection: close\r\n\r\n"
        );
        let response = ctx.raw(&request).await;
        if status(&response) == 400 {
            return Ok(());
        }
        let forwarded = ctx
            .upstream
            .request("/folded")
            .await
            .ok_or("not forwarded, nor refused")?;
        let value = forwarded.all("x-folded").join(",");
        ensure(!value.contains('\r') && !value.contains('\n'), || {
            format!("forwarded folded: {value:?}")
        })
    })
}

fn bare_cr(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        let request = format!(
            "GET /bare-cr HTTP/1.1\r\nHost: {PLAIN}\r\nX-Cr: a\rb\r\nConnection: close\r\n\r\n"
        );
        refused(ctx, "/bare-cr", &request).await
    })
}

fn no_host(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        refused(
            ctx,
            "/no-host",
            "GET /no-host HTTP/1.1\r\nConnection: close\r\n\r\n",
        )
        .await
    })
}

fn two_hosts(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        let request = format!(
            "GET /two-hosts HTTP/1.1\r\nHost: {PLAIN}\r\nHost: other.test\r\nConnection: close\r\n\r\n"
        );
        refused(ctx, "/two-hosts", &request).await
    })
}

fn absolute_form(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        let request = format!(
            "GET http://{PLAIN}/absolute HTTP/1.1\r\nHost: other.test\r\nConnection: close\r\n\r\n"
        );
        let response = ctx.raw(&request).await;
        ensure(status(&response) == 200, || {
            format!("got {:?}", head(&response))
        })?;
        let forwarded = ctx.upstream.request("/absolute").await;
        ensure(forwarded.is_some(), || "not forwarded".into())
    })
}

fn connection_close(ctx: &Ctx) -> Check<'_> {
    Box::pin(async move {
        // `raw` reads until the connection is closed: it times out otherwise
        let request = format!("GET /close HTTP/1.1\r\nHost: {PLAIN}\r\nConnection: close\r\n\r\n");
        let response = ctx.raw(&request).await;
        ensure(status(&response) == 200, || {
            format!("got {:?}", head(&response))
        })
    })
}
