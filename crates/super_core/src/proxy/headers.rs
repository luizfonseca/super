//! What changes between a client's request (or an upstream's response) and the one sent on:
//! the headers of a connection stay with it

use http::{header, HeaderMap, HeaderName, Uri, Version};

use crate::http::{RequestHeader, ResponseHeader};

/// Headers of a connection, never passed on (RFC 9110, 7.6.1)
const HOP_BY_HOP: [HeaderName; 5] = [
    header::CONNECTION,
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
];

const KEEP_ALIVE: &str = "keep-alive";
const PROXY_CONNECTION: &str = "proxy-connection";

/// Removes the headers of the connection; those of an upgrade are kept when `upgrade`
fn remove_hop_by_hop(headers: &mut HeaderMap, upgrade: bool) {
    // Headers the `Connection` header names are the connection's too
    if !upgrade {
        let named = headers
            .get_all(header::CONNECTION)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
            .filter(|name| name != header::CONNECTION && !is_hop_by_hop(name))
            .collect::<Vec<_>>();
        for name in named {
            headers.remove(name);
        }
    }
    for name in &HOP_BY_HOP {
        let kept = upgrade && (name == header::CONNECTION || name == header::UPGRADE);
        if !kept {
            headers.remove(name);
        }
    }
    headers.remove(KEEP_ALIVE);
    headers.remove(PROXY_CONNECTION);
}

fn is_hop_by_hop(name: &HeaderName) -> bool {
    HOP_BY_HOP.contains(name)
}

/// The request sent to an upstream: `request`, without its connection's headers, for
/// HTTP/1.1 (`http2`: for HTTP/2)
pub(crate) fn upstream_request(
    request: &RequestHeader,
    upgrade: bool,
    http2: bool,
) -> RequestHeader {
    let mut upstream = request.clone();
    // Header names a connection lists are rarely there: only then is `Connection` parsed
    if upstream.headers.contains_key(header::CONNECTION) || upgrade {
        remove_hop_by_hop(&mut upstream.headers, upgrade);
    } else {
        for name in &HOP_BY_HOP {
            upstream.headers.remove(name);
        }
        upstream.headers.remove(KEEP_ALIVE);
        upstream.headers.remove(PROXY_CONNECTION);
    }
    set_target(&mut upstream, http2);
    upstream
}

/// The request's target, as its version wants it: a path (HTTP/1.1, with the host in `Host`),
/// or the whole URI (HTTP/2)
pub fn set_target(request: &mut RequestHeader, http2: bool) {
    if http2 {
        http2_target(request);
        return;
    }
    request.set_version(Version::HTTP_11);
    let Some(authority) = request.uri.authority().cloned() else {
        return;
    };
    // An HTTP/2 client's URI: its host goes to `Host`
    if !request.headers.contains_key(header::HOST) {
        if let Ok(host) = http::HeaderValue::from_str(authority.as_str()) {
            request.headers.insert(header::HOST, host);
        }
    }
    let path = request
        .uri
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| http::uri::PathAndQuery::from_static("/"));
    request.set_uri(Uri::from(path));
}

/// The whole URI, as HTTP/2 wants it: its host goes to `:authority`, from `Host` when the client
/// sent a path
fn http2_target(request: &mut RequestHeader) {
    request.set_version(Version::HTTP_2);
    if request.uri.authority().is_none() {
        let host = request
            .headers
            .get(header::HOST)
            .and_then(|host| host.to_str().ok())
            .unwrap_or("localhost");
        let path = request
            .uri
            .path_and_query()
            .map_or("/", http::uri::PathAndQuery::as_str);
        let uri = Uri::builder()
            .scheme("http")
            .authority(host)
            .path_and_query(path)
            .build();
        if let Ok(uri) = uri {
            request.set_uri(uri);
        }
    }
    request.headers.remove(header::HOST);
}

/// The upstream's response, without its connection's headers (`upgrade`: a `101`)
pub(crate) fn downstream_response(response: &mut ResponseHeader, upgrade: bool) {
    if response.headers.contains_key(header::CONNECTION) || upgrade {
        remove_hop_by_hop(&mut response.headers, upgrade);
        return;
    }
    // Transfer-Encoding is hyper's to choose, for the client's connection
    response.headers.remove(header::TRANSFER_ENCODING);
    response.headers.remove(header::TRAILER);
    response.headers.remove(KEEP_ALIVE);
    response.headers.remove(PROXY_CONNECTION);
}

#[cfg(test)]
mod tests {
    use http::header;

    use super::{downstream_response, upstream_request};
    use crate::http::{RequestHeader, ResponseHeader};

    #[test]
    fn test_connection_headers_stay_with_their_connection() {
        let mut request = RequestHeader::build("GET", b"/a?b", None).unwrap();
        request.insert_header(header::HOST, "a.test").unwrap();
        request
            .insert_header(header::CONNECTION, "keep-alive, x-private")
            .unwrap();
        request.insert_header("x-private", "1").unwrap();
        request.insert_header("keep-alive", "timeout=5").unwrap();
        request.insert_header("x-kept", "1").unwrap();

        let upstream = upstream_request(&request, false, false);
        assert!(!upstream.headers.contains_key(header::CONNECTION));
        assert!(!upstream.headers.contains_key("x-private"));
        assert!(!upstream.headers.contains_key("keep-alive"));
        assert_eq!(upstream.headers["x-kept"], "1");
        assert_eq!(upstream.uri, "/a?b");
    }

    #[test]
    fn test_upgrades_keep_their_headers() {
        let mut request = RequestHeader::build("GET", b"/ws", None).unwrap();
        request
            .insert_header(header::CONNECTION, "Upgrade")
            .unwrap();
        request.insert_header(header::UPGRADE, "websocket").unwrap();
        let upstream = upstream_request(&request, true, false);
        assert_eq!(upstream.headers[header::UPGRADE], "websocket");
        assert_eq!(upstream.headers[header::CONNECTION], "Upgrade");
    }

    #[test]
    fn test_http2_clients_and_upstreams() {
        // An HTTP/2 client's request, to an HTTP/1.1 upstream
        let request = RequestHeader::build("GET", b"https://a.test/p?q", None).unwrap();
        let upstream = upstream_request(&request, false, false);
        assert_eq!(upstream.uri, "/p?q");
        assert_eq!(upstream.headers[header::HOST], "a.test");

        // An HTTP/1.1 client's request, to an HTTP/2 upstream
        let mut request = RequestHeader::build("GET", b"/p", None).unwrap();
        request.insert_header(header::HOST, "b.test").unwrap();
        let upstream = upstream_request(&request, false, true);
        assert_eq!(upstream.uri, "http://b.test/p");
        assert!(!upstream.headers.contains_key(header::HOST));

        let mut response = ResponseHeader::build(200, None).unwrap();
        response
            .insert_header(header::TRANSFER_ENCODING, "chunked")
            .unwrap();
        downstream_response(&mut response, false);
        assert!(!response.headers.contains_key(header::TRANSFER_ENCODING));
    }
}
