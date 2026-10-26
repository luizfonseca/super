//! Responses compressed for the client, with the algorithm it accepts (as Pingora's response
//! compression): off until a level is set for an algorithm

use std::io::Write;

use bytes::Bytes;
use http::{header, HeaderValue};

use crate::http::{RequestHeader, ResponseHeader};

/// Responses shorter than this aren't worth compressing
const MIN_COMPRESS_LEN: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Any,
    Gzip,
    Brotli,
    Zstd,
}

impl Algorithm {
    const ALL: [Algorithm; 3] = [Algorithm::Gzip, Algorithm::Brotli, Algorithm::Zstd];

    pub fn as_str(self) -> &'static str {
        match self {
            Algorithm::Gzip => "gzip",
            Algorithm::Brotli => "br",
            Algorithm::Zstd => "zstd",
            Algorithm::Any => "*",
        }
    }

    fn index(self) -> Option<usize> {
        Self::ALL.iter().position(|algorithm| *algorithm == self)
    }

    fn named(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|algorithm| algorithm.as_str().eq_ignore_ascii_case(name))
    }
}

/// Compresses a body as it's written
enum Encoder {
    Gzip(flate2::write::GzEncoder<Vec<u8>>),
    Brotli(Box<brotli::CompressorWriter<Vec<u8>>>),
    Zstd(zstd::stream::write::Encoder<'static, Vec<u8>>),
}

impl Encoder {
    fn new(algorithm: Algorithm, level: u32) -> Option<Self> {
        match algorithm {
            Algorithm::Gzip => Some(Encoder::Gzip(flate2::write::GzEncoder::new(
                Vec::new(),
                flate2::Compression::new(level),
            ))),
            Algorithm::Brotli => Some(Encoder::Brotli(Box::new(brotli::CompressorWriter::new(
                Vec::new(),
                4096,
                level,
                22,
            )))),
            Algorithm::Zstd => {
                zstd::stream::write::Encoder::new(Vec::new(), i32::try_from(level).unwrap_or(3))
                    .ok()
                    .map(Encoder::Zstd)
            }
            Algorithm::Any => None,
        }
    }

    /// What `input` compresses to so far (all of it with `end`)
    fn encode(&mut self, input: &[u8], end: bool) -> std::io::Result<Bytes> {
        let output = match self {
            Encoder::Gzip(encoder) => {
                encoder.write_all(input)?;
                if end {
                    encoder.try_finish()?;
                }
                encoder.get_mut()
            }
            Encoder::Brotli(encoder) => {
                encoder.write_all(input)?;
                if end {
                    encoder.flush()?;
                    // Ends the stream (a brotli writer ends when it's done with)
                    let finished = std::mem::replace(
                        encoder,
                        Box::new(brotli::CompressorWriter::new(Vec::new(), 4096, 0, 22)),
                    );
                    return Ok(Bytes::from(finished.into_inner()));
                }
                encoder.get_mut()
            }
            Encoder::Zstd(encoder) => {
                encoder.write_all(input)?;
                if end {
                    encoder.do_finish()?;
                }
                encoder.get_mut()
            }
        };
        Ok(Bytes::from(std::mem::take(output)))
    }
}

/// The compression of a request's response
#[derive(Default)]
pub struct ResponseCompression {
    /// The level of each algorithm (`0`: off)
    levels: [u32; 3],
    /// What the client accepts, best first
    accepted: Option<Algorithm>,
    encoder: Option<Encoder>,
}

impl ResponseCompression {
    pub fn new(level: u32) -> Self {
        Self {
            levels: [level; 3],
            ..Self::default()
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.levels.iter().any(|level| *level > 0)
    }

    /// The level of `algorithm` (`0` turns it off; `Any` sets every algorithm)
    pub fn adjust_algorithm_level(&mut self, algorithm: Algorithm, level: u32) {
        match algorithm.index() {
            Some(index) => {
                if let Some(slot) = self.levels.get_mut(index) {
                    *slot = level;
                }
            }
            None => self.levels = [level; 3],
        }
    }

    /// The first algorithm of the request's `Accept-Encoding` that's on
    pub fn request_filter(&mut self, request: &RequestHeader) {
        let Some(accept) = request
            .headers
            .get(header::ACCEPT_ENCODING)
            .and_then(|value| value.to_str().ok())
        else {
            return;
        };
        self.accepted = accept
            .split(',')
            .filter_map(|coding| Algorithm::named(coding.split(';').next()?.trim()))
            .find(|algorithm| self.level(*algorithm) > 0);
    }

    fn level(&self, algorithm: Algorithm) -> u32 {
        algorithm
            .index()
            .and_then(|index| self.levels.get(index).copied())
            .unwrap_or(0)
    }

    /// Compresses the response if it's worth it: its header says so
    pub fn response_header_filter(&mut self, response: &mut ResponseHeader, end: bool) {
        if !self.is_enabled() || end || response.status.is_informational() {
            return;
        }
        add_vary(response);
        let Some(algorithm) = self.accepted.filter(|_| compressible(response)) else {
            return;
        };
        let Some(encoder) = Encoder::new(algorithm, self.level(algorithm)) else {
            return;
        };
        self.encoder = Some(encoder);
        response.remove_header(&header::CONTENT_LENGTH);
        response.remove_header(&header::ACCEPT_RANGES);
        let _ = response.insert_header(header::CONTENT_ENCODING, algorithm.as_str());
        // The compressed body isn't the same bytes
        weaken_etag(response);
    }

    /// A part of the body, compressed when the response is
    pub fn response_body_filter(&mut self, body: Option<&Bytes>, end: bool) -> Option<Bytes> {
        let encoder = self.encoder.as_mut()?;
        let input = body.map_or(&[][..], |body| body.as_ref());
        match encoder.encode(input, end) {
            Ok(output) => Some(output),
            Err(err) => {
                tracing::warn!("compressing a response failed: {err}");
                Some(Bytes::new())
            }
        }
    }

    /// Whether the response is being compressed
    pub fn is_compressing(&self) -> bool {
        self.encoder.is_some()
    }
}

/// Whether the response is worth compressing: a type that compresses well, not compressed
/// already, and not tiny
fn compressible(response: &ResponseHeader) -> bool {
    if response.headers.contains_key(header::CONTENT_ENCODING) {
        return false;
    }
    let length = response
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok()?.parse::<usize>().ok());
    if length.is_some_and(|length| length < MIN_COMPRESS_LEN) {
        return false;
    }
    let Some(content_type) = response
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    if content_type.contains("zip") {
        return false;
    }
    let content_type = content_type.trim_start().to_ascii_lowercase();
    ["text/", "application/", "font/", "binary/octet-stream"]
        .iter()
        .any(|prefix| content_type.starts_with(prefix))
        || ["image/x-icon", "image/svg+xml", "image/nd.microsoft.icon"]
            .iter()
            .any(|kind| content_type.starts_with(kind))
}

fn add_vary(response: &mut ResponseHeader) {
    let present = response.headers.get_all(header::VARY).iter().any(|value| {
        value
            .as_bytes()
            .split(|byte| *byte == b',')
            .map(<[u8]>::trim_ascii)
            .any(|name| name == b"*" || name.eq_ignore_ascii_case(b"accept-encoding"))
    });
    if !present {
        response
            .headers
            .append(header::VARY, HeaderValue::from_static("accept-encoding"));
    }
}

fn weaken_etag(response: &mut ResponseHeader) {
    let Some(etag) = response.headers.get(header::ETAG) else {
        return;
    };
    if etag.as_bytes().starts_with(b"W/") {
        return;
    }
    let mut weak = b"W/".to_vec();
    weak.extend_from_slice(etag.as_bytes());
    if let Ok(weak) = HeaderValue::from_bytes(&weak) {
        response.headers.insert(header::ETAG, weak);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use bytes::Bytes;
    use http::header;

    use super::{Algorithm, ResponseCompression};
    use crate::http::{RequestHeader, ResponseHeader};

    fn response(content_type: &str) -> ResponseHeader {
        let mut response = ResponseHeader::build(200, None).unwrap();
        response
            .insert_header(header::CONTENT_TYPE, content_type)
            .unwrap();
        response
            .insert_header(header::CONTENT_LENGTH, 1000)
            .unwrap();
        response.insert_header(header::ETAG, "\"v1\"").unwrap();
        response
    }

    fn compression(accept: &str) -> ResponseCompression {
        let mut compression = ResponseCompression::default();
        compression.adjust_algorithm_level(Algorithm::Gzip, 6);
        let mut request = RequestHeader::build("GET", b"/", None).unwrap();
        request
            .insert_header(header::ACCEPT_ENCODING, accept)
            .unwrap();
        compression.request_filter(&request);
        compression
    }

    #[test]
    fn test_responses_compressed_as_accepted() {
        let mut compression = compression("br, gzip");
        let mut header = response("text/html; charset=utf-8");
        compression.response_header_filter(&mut header, false);
        assert_eq!(header.headers[header::CONTENT_ENCODING], "gzip");
        assert!(!header.headers.contains_key(header::CONTENT_LENGTH));
        assert_eq!(header.headers[header::ETAG], "W/\"v1\"");
        assert_eq!(header.headers[header::VARY], "accept-encoding");

        let text = "hello ".repeat(100);
        let mut compressed = compression
            .response_body_filter(Some(&Bytes::from(text.clone())), false)
            .unwrap()
            .to_vec();
        compressed.extend_from_slice(&compression.response_body_filter(None, true).unwrap());
        let mut decompressed = String::new();
        flate2::read::GzDecoder::new(&compressed[..])
            .read_to_string(&mut decompressed)
            .unwrap();
        assert_eq!(decompressed, text);
    }

    #[test]
    fn test_responses_left_as_they_are() {
        // Brotli is off, images don't compress well, archives are compressed already
        for (accept, content_type) in [
            ("br", "text/html"),
            ("gzip", "image/png"),
            ("gzip", "application/zip"),
        ] {
            let mut compression = compression(accept);
            let mut header = response(content_type);
            compression.response_header_filter(&mut header, false);
            assert!(!compression.is_compressing(), "{accept} {content_type}");
            assert!(header.headers.contains_key(header::CONTENT_LENGTH));
        }
    }
}
