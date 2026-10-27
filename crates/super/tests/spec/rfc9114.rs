//! RFC 9114, HTTP/3: a request of an HTTP/3 client, as a proxy forwards it to HTTP/1.1

use crate::{harness::Client, Check, Ctx, Level::Must, Requirement};

pub fn rows() -> Vec<Requirement> {
    vec![Requirement {
        rfc: 9114,
        section: "4.2.1",
        level: Must,
        says:
            "an HTTP/3 request's Cookie fields reach an HTTP/1.1 upstream as one, joined by \"; \"",
        check: cookies,
    }]
}

fn cookies(ctx: &Ctx) -> Check<'_> {
    Box::pin(super::rfc9113::joined_cookies(
        ctx,
        Client::Http3,
        "/cookies-h3",
    ))
}
