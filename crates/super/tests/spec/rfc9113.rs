//! RFC 9113, HTTP/2: a request of an HTTP/2 client, as a proxy forwards it to HTTP/1.1

use crate::{ensure, harness::Client, Check, Ctx, Level::Must, Requirement};

pub fn rows() -> Vec<Requirement> {
    vec![Requirement {
        rfc: 9113,
        section: "8.2.3",
        level: Must,
        says:
            "an HTTP/2 request's Cookie fields reach an HTTP/1.1 upstream as one, joined by \"; \"",
        check: cookies,
    }]
}

fn cookies(ctx: &Ctx) -> Check<'_> {
    Box::pin(joined_cookies(ctx, Client::Http2, "/cookies-h2"))
}

/// A request of `client` with two `Cookie` fields reaches the upstream with one
pub async fn joined_cookies(ctx: &Ctx, client: Client, path: &str) -> Result<(), String> {
    ctx.send(client, "GET", path, &[("cookie", "a=1"), ("cookie", "b=2")])
        .await;
    let forwarded = ctx.upstream.request(path).await.ok_or("not forwarded")?;
    let cookies = forwarded.all("cookie");
    ensure(cookies == ["a=1; b=2"], || format!("forwarded {cookies:?}"))
}
