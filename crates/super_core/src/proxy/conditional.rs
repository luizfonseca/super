//! Conditional and range requests answered from the cache (RFC 9110, 13 and 14): a client whose
//! copy is still the cached one gets a `304`, one asking for a part of the body gets it (`206`),
//! and a stale response is revalidated with the upstream with its validators

use http::{header, HeaderMap, HeaderValue, StatusCode};

use crate::http::{RequestHeader, ResponseHeader};

/// Whether the client's copy is the cached response (`If-None-Match`, else `If-Modified-Since`)
pub(super) fn not_modified(request: &RequestHeader, response: &ResponseHeader) -> bool {
    if let Some(wanted) = request.headers.get(header::IF_NONE_MATCH) {
        let Some(etag) = response.headers.get(header::ETAG) else {
            return false;
        };
        return etag_matches(wanted, etag);
    }
    let since = request
        .headers
        .get(header::IF_MODIFIED_SINCE)
        .and_then(|value| httpdate::parse_http_date(value.to_str().ok()?).ok());
    let modified = response
        .headers
        .get(header::LAST_MODIFIED)
        .and_then(|value| httpdate::parse_http_date(value.to_str().ok()?).ok());
    since
        .zip(modified)
        .is_some_and(|(since, modified)| modified <= since)
}

/// Whether `etag` is one of `wanted` (`*` or a list), compared weakly as `If-None-Match` does
fn etag_matches(wanted: &HeaderValue, etag: &HeaderValue) -> bool {
    let weak = |tag: &str| tag.trim().trim_start_matches("W/").to_string();
    let Ok(etag) = etag.to_str() else {
        return false;
    };
    let etag = weak(etag);
    wanted
        .to_str()
        .is_ok_and(|wanted| wanted.trim() == "*" || wanted.split(',').any(|tag| weak(tag) == etag))
}

/// The response, as a `304`: its validators and caching headers, without a body
pub(super) fn to_not_modified(response: &mut ResponseHeader) {
    response.status = StatusCode::NOT_MODIFIED;
    for name in [
        header::CONTENT_LENGTH,
        header::CONTENT_TYPE,
        header::CONTENT_ENCODING,
        header::CONTENT_RANGE,
        header::TRANSFER_ENCODING,
    ] {
        response.headers.remove(name);
    }
}

/// A part of the body a client asks for
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Range {
    /// From `start` to `end` (included), of a body of `length` bytes
    Part { start: u64, end: u64, length: u64 },
    /// Beyond the body (`416`)
    Unsatisfiable { length: u64 },
}

/// The single byte range the client asks for, of a whole `200` response of known length. `None`
/// for the whole body: no range, several, `If-Range`, or one that can't be read.
pub(super) fn range(request: &RequestHeader, response: &ResponseHeader) -> Option<Range> {
    if response.status != StatusCode::OK || request.headers.contains_key(header::IF_RANGE) {
        return None;
    }
    let length = response
        .headers
        .get(header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()?;
    let spec = request.headers.get(header::RANGE)?.to_str().ok()?;
    let spec = spec.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (first, last) = spec.split_once('-')?;
    let (first, last) = (first.trim(), last.trim());
    let (start, end) = match (first.is_empty(), last.is_empty()) {
        // The last `n` bytes
        (true, false) => {
            let suffix = last.parse::<u64>().ok()?;
            (length.saturating_sub(suffix), length.checked_sub(1)?)
        }
        (false, true) => (first.parse::<u64>().ok()?, length.checked_sub(1)?),
        (false, false) => (
            first.parse().ok()?,
            last.parse::<u64>().ok()?.min(length.checked_sub(1)?),
        ),
        (true, true) => return None,
    };
    if start >= length || start > end {
        return Some(Range::Unsatisfiable { length });
    }
    Some(Range::Part { start, end, length })
}

/// The response, as an answer to `range`
pub(super) fn to_range(response: &mut ResponseHeader, range: &Range) {
    let (status, content_range, content_length) = match *range {
        Range::Part { start, end, length } => (
            StatusCode::PARTIAL_CONTENT,
            format!("bytes {start}-{end}/{length}"),
            end - start + 1,
        ),
        Range::Unsatisfiable { length } => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            format!("bytes */{length}"),
            0,
        ),
    };
    response.status = status;
    if let Ok(value) = HeaderValue::from_str(&content_range) {
        response.headers.insert(header::CONTENT_RANGE, value);
    }
    response
        .headers
        .insert(header::CONTENT_LENGTH, HeaderValue::from(content_length));
}

/// Asks the upstream whether the stale `cached` response changed: its validators, unless the
/// client asked a question of its own. Returns whether it asked.
pub(super) fn ask_upstream(request: &mut RequestHeader, cached: &HeaderMap) -> bool {
    let conditional = [header::IF_NONE_MATCH, header::IF_MODIFIED_SINCE];
    if conditional
        .iter()
        .any(|name| request.headers.contains_key(name))
    {
        return false;
    }
    let mut asked = false;
    if let Some(etag) = cached.get(header::ETAG) {
        request.headers.insert(header::IF_NONE_MATCH, etag.clone());
        asked = true;
    }
    if let Some(modified) = cached.get(header::LAST_MODIFIED) {
        request
            .headers
            .insert(header::IF_MODIFIED_SINCE, modified.clone());
        asked = true;
    }
    asked
}

/// The cached response's header, updated by the upstream's `304` (RFC 9111, 4.3.4): its headers
/// replace the cached ones, but those of the body
pub(super) fn merge(cached: &ResponseHeader, not_modified: &ResponseHeader) -> ResponseHeader {
    let mut merged = cached.clone();
    let body = [
        header::CONTENT_LENGTH,
        header::CONTENT_TYPE,
        header::CONTENT_ENCODING,
        header::CONTENT_RANGE,
        header::TRANSFER_ENCODING,
    ];
    for name in not_modified.headers.keys() {
        if body.contains(name) {
            continue;
        }
        merged.headers.remove(name);
        for value in not_modified.headers.get_all(name) {
            merged.headers.append(name.clone(), value.clone());
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use http::header;

    use super::{ask_upstream, merge, not_modified, range, to_range, Range};
    use crate::http::{RequestHeader, ResponseHeader};

    fn request(headers: &[(&str, &str)]) -> RequestHeader {
        let mut request = RequestHeader::build("GET", b"/", None).unwrap();
        for (name, value) in headers {
            request.insert_header(*name, *value).unwrap();
        }
        request
    }

    fn response() -> ResponseHeader {
        let mut response = ResponseHeader::build(200, None).unwrap();
        response.insert_header(header::ETAG, "\"v1\"").unwrap();
        response
            .insert_header(header::LAST_MODIFIED, "Mon, 26 Oct 2026 10:00:00 GMT")
            .unwrap();
        response.insert_header(header::CONTENT_LENGTH, 100).unwrap();
        response
            .insert_header(header::CONTENT_TYPE, "text/plain")
            .unwrap();
        response
    }

    #[test]
    fn test_copies_still_current() {
        let cached = response();
        assert!(not_modified(
            &request(&[("if-none-match", "\"v1\"")]),
            &cached
        ));
        assert!(not_modified(
            &request(&[("if-none-match", "\"v0\", W/\"v1\"")]),
            &cached
        ));
        assert!(not_modified(&request(&[("if-none-match", "*")]), &cached));
        assert!(!not_modified(
            &request(&[("if-none-match", "\"v2\"")]),
            &cached
        ));
        let since = |date| request(&[("if-modified-since", date)]);
        assert!(not_modified(
            &since("Mon, 26 Oct 2026 10:00:00 GMT"),
            &cached
        ));
        assert!(!not_modified(
            &since("Mon, 26 Oct 2026 09:00:00 GMT"),
            &cached
        ));
        assert!(!not_modified(&request(&[]), &cached));
    }

    #[test]
    fn test_ranges() {
        let cached = response();
        let part = |spec| range(&request(&[("range", spec)]), &cached);
        let piece = |start, end| {
            Some(Range::Part {
                start,
                end,
                length: 100,
            })
        };
        assert_eq!(part("bytes=0-9"), piece(0, 9));
        assert_eq!(part("bytes=90-"), piece(90, 99));
        assert_eq!(part("bytes=-10"), piece(90, 99));
        assert_eq!(part("bytes=95-200"), piece(95, 99));
        assert_eq!(
            part("bytes=100-"),
            Some(Range::Unsatisfiable { length: 100 })
        );
        // The whole body: several ranges, or unknown units
        assert_eq!(part("bytes=0-1,5-6"), None);
        assert_eq!(part("items=0-1"), None);
        let if_range = request(&[("range", "bytes=0-9"), ("if-range", "\"v1\"")]);
        assert_eq!(range(&if_range, &cached), None);

        let mut answer = response();
        to_range(
            &mut answer,
            &Range::Part {
                start: 90,
                end: 99,
                length: 100,
            },
        );
        assert_eq!(answer.status, 206);
        assert_eq!(answer.headers[header::CONTENT_RANGE], "bytes 90-99/100");
        assert_eq!(answer.headers[header::CONTENT_LENGTH], "10");
    }

    #[test]
    fn test_revalidation_with_the_upstream() {
        let cached = response();
        let mut upstream = request(&[]);
        assert!(ask_upstream(&mut upstream, &cached.headers));
        assert_eq!(upstream.headers[header::IF_NONE_MATCH], "\"v1\"");
        // A client's own question goes as it is
        let mut asking = request(&[("if-none-match", "\"v0\"")]);
        assert!(!ask_upstream(&mut asking, &cached.headers));
        assert_eq!(asking.headers[header::IF_NONE_MATCH], "\"v0\"");

        let mut not_modified = ResponseHeader::build(304, None).unwrap();
        not_modified
            .insert_header(header::CACHE_CONTROL, "max-age=60")
            .unwrap();
        not_modified
            .insert_header(header::CONTENT_LENGTH, 0)
            .unwrap();
        let merged = merge(&cached, &not_modified);
        assert_eq!(merged.status, 200);
        assert_eq!(merged.headers[header::CACHE_CONTROL], "max-age=60");
        assert_eq!(merged.headers[header::CONTENT_LENGTH], "100");
        assert_eq!(merged.headers[header::ETAG], "\"v1\"");
    }
}
