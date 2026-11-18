//! The PROXY protocol (v1 and v2): the header a load balancer sends first on a connection,
//! with the address of the client it accepted the connection from

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// How a v1 header starts
const V1: &[u8] = b"PROXY ";
/// The longest a v1 header can be, its `\r\n` included
const V1_MAX: usize = 107;
/// How a v2 header starts
const V2: &[u8] = b"\r\n\r\n\0\r\nQUIT\n";
/// The fixed part of a v2 header: its signature, version and command, family, and length
const V2_FIXED: usize = 16;

/// What the first bytes of a connection are
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parsed {
    /// Not a PROXY protocol header: the connection's own bytes
    NotProxy,
    /// Maybe a header: more bytes are needed to tell
    Incomplete,
    /// A header of `len` bytes, from `source` (`None`: the load balancer's own connection, e.g.
    /// a health check, or a protocol other than TCP)
    Header {
        len: usize,
        source: Option<SocketAddr>,
    },
    /// A header that doesn't follow the protocol
    Invalid,
}

/// What `bytes`, the first of a connection, are. Told from its first bytes when it's not a
/// header (an HTTP request, a TLS `ClientHello`...), without waiting for more.
pub fn parse(bytes: &[u8]) -> Parsed {
    if starts_like(bytes, V1) {
        return parse_v1(bytes);
    }
    if starts_like(bytes, V2) {
        return parse_v2(bytes);
    }
    Parsed::NotProxy
}

/// Whether `bytes` are `signature`, or the start of it
fn starts_like(bytes: &[u8], signature: &[u8]) -> bool {
    let len = bytes.len().min(signature.len());
    bytes.get(..len) == signature.get(..len)
}

/// `PROXY TCP4 <source> <destination> <source port> <destination port>\r\n` (or `TCP6`), or
/// `PROXY UNKNOWN ...\r\n`
fn parse_v1(bytes: &[u8]) -> Parsed {
    let searched = bytes.get(..bytes.len().min(V1_MAX)).unwrap_or_default();
    let Some(end) = searched.iter().position(|byte| *byte == b'\n') else {
        if bytes.len() >= V1_MAX {
            return Parsed::Invalid;
        }
        return Parsed::Incomplete;
    };
    // `\r\n` ends it, not `\n` alone
    let line = end
        .checked_sub(1)
        .filter(|cr| searched.get(*cr) == Some(&b'\r'))
        .and_then(|cr| searched.get(..cr))
        .and_then(|line| std::str::from_utf8(line).ok());
    let Some(line) = line else {
        return Parsed::Invalid;
    };
    let len = end + 1;
    if line.split(' ').nth(1) == Some("UNKNOWN") {
        return Parsed::Header { len, source: None };
    }
    match v1_source(line) {
        Some(source) => Parsed::Header {
            len,
            source: Some(source),
        },
        None => Parsed::Invalid,
    }
}

/// The source of a v1 header's `line` (without its `\r\n`), for TCP: `None` when it's invalid
fn v1_source(line: &str) -> Option<SocketAddr> {
    let mut fields = line.split(' ').skip(1);
    let protocol = fields.next()?;
    let (source, destination) = (fields.next()?, fields.next()?);
    let (port, destination_port) = (fields.next()?, fields.next()?);
    if fields.next().is_some() {
        return None;
    }
    let ip: IpAddr = match protocol {
        "TCP4" => {
            destination.parse::<Ipv4Addr>().ok()?;
            source.parse::<Ipv4Addr>().ok()?.into()
        }
        "TCP6" => {
            destination.parse::<Ipv6Addr>().ok()?;
            source.parse::<Ipv6Addr>().ok()?.into()
        }
        _ => return None,
    };
    destination_port.parse::<u16>().ok()?;
    Some(SocketAddr::new(ip, port.parse().ok()?))
}

/// The signature, `0x2_` (version 2, command: `0` LOCAL or `1` PROXY), the family (`0x11`: TCP
/// over IPv4, `0x21`: TCP over IPv6...), the length of what follows (addresses, then TLVs)
fn parse_v2(bytes: &[u8]) -> Parsed {
    let Some(fixed) = bytes.get(..V2_FIXED) else {
        return Parsed::Incomplete;
    };
    let [.., version_command, family, len_high, len_low] = *fixed else {
        return Parsed::Incomplete;
    };
    let len = V2_FIXED + usize::from(u16::from_be_bytes([len_high, len_low]));
    let Some(addresses) = bytes.get(V2_FIXED..len) else {
        return Parsed::Incomplete;
    };
    let command = version_command & 0x0f;
    if version_command >> 4 != 2 || command > 1 || family >> 4 > 3 || family & 0x0f > 2 {
        return Parsed::Invalid;
    }
    let size = match (command, family) {
        (1, 0x11) => 4,
        (1, 0x21) => 16,
        // The load balancer's own connection (LOCAL), or another protocol than TCP: no client
        _ => return Parsed::Header { len, source: None },
    };
    match v2_source(addresses, size) {
        Some(source) => Parsed::Header {
            len,
            source: Some(source),
        },
        None => Parsed::Invalid,
    }
}

/// The source of a v2 header's `addresses`, of `size` bytes each: source, destination, source
/// port, destination port
fn v2_source(addresses: &[u8], size: usize) -> Option<SocketAddr> {
    let ip = addresses.get(..size)?;
    let port = addresses.get(size * 2..size * 2 + 2)?;
    let ip = match size {
        4 => IpAddr::from(<[u8; 4]>::try_from(ip).ok()?),
        _ => IpAddr::from(<[u8; 16]>::try_from(ip).ok()?),
    };
    let port = u16::from_be_bytes(<[u8; 2]>::try_from(port).ok()?);
    // The destination port, which must be there too
    addresses.get(size * 2 + 3)?;
    Some(SocketAddr::new(ip, port))
}

#[cfg(test)]
mod tests {
    use super::{parse, Parsed};

    /// The signature of a v2 header
    const V2: &[u8] = b"\r\n\r\n\0\r\nQUIT\n";

    fn header(len: usize, source: &str) -> Parsed {
        Parsed::Header {
            len,
            source: Some(source.parse().unwrap()),
        }
    }

    /// A v2 header: `command` (0: LOCAL, 1: PROXY), `family` (0x11: TCP over IPv4...), its
    /// addresses, then the connection's own bytes
    fn v2(command: u8, family: u8, addresses: &[u8], rest: &[u8]) -> Vec<u8> {
        let mut bytes = V2.to_vec();
        bytes.push(0x20 | command);
        bytes.push(family);
        bytes.extend_from_slice(&u16::try_from(addresses.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(addresses);
        bytes.extend_from_slice(rest);
        bytes
    }

    #[test]
    fn test_connections_without_a_header() {
        // Told apart from their first byte: HTTP, a TLS ClientHello
        assert_eq!(parse(b"GET / HTTP/1.1\r\n"), Parsed::NotProxy);
        assert_eq!(parse(b"\x16\x03\x01\x02\x00"), Parsed::NotProxy);
        assert_eq!(parse(b"PRI * HTTP/2.0"), Parsed::NotProxy);
        assert_eq!(parse(b"\r\nfoo"), Parsed::NotProxy);
        // Nothing yet
        assert_eq!(parse(b""), Parsed::Incomplete);
    }

    #[test]
    fn test_v1_headers() {
        let tcp4 = b"PROXY TCP4 203.0.113.7 10.0.0.1 51234 443\r\nGET /";
        assert_eq!(parse(tcp4), header(43, "203.0.113.7:51234"));
        let tcp6 = b"PROXY TCP6 2001:db8::7 2001:db8::1 51234 443\r\n";
        assert_eq!(parse(tcp6), header(tcp6.len(), "[2001:db8::7]:51234"));
        // A connection of the load balancer's own: no client
        let unknown = b"PROXY UNKNOWN\r\n";
        assert_eq!(
            parse(unknown),
            Parsed::Header {
                len: unknown.len(),
                source: None
            }
        );
        let unknown = b"PROXY UNKNOWN ffff::1 ffff::2 1 2\r\n";
        assert_eq!(
            parse(unknown),
            Parsed::Header {
                len: unknown.len(),
                source: None
            }
        );
    }

    #[test]
    fn test_v1_headers_arriving_in_pieces() {
        let full = b"PROXY TCP4 203.0.113.7 10.0.0.1 51234 443\r\n";
        for end in 0..full.len() {
            assert_eq!(parse(&full[..end]), Parsed::Incomplete, "{end} bytes");
        }
        assert_eq!(parse(full), header(full.len(), "203.0.113.7:51234"));
    }

    #[test]
    fn test_invalid_v1_headers() {
        for invalid in [
            &b"PROXY TCP4 203.0.113.7 10.0.0.1 51234\r\n"[..],
            b"PROXY TCP4 2001:db8::7 10.0.0.1 51234 443\r\n",
            b"PROXY TCP6 203.0.113.7 10.0.0.1 51234 443\r\n",
            b"PROXY TCP4 203.0.113.7 10.0.0.1 99999 443\r\n",
            b"PROXY UDP4 203.0.113.7 10.0.0.1 51234 443\r\n",
            b"PROXY TCP4 203.0.113.7 10.0.0.1 51234 443\n",
            b"PROXY TCP4  203.0.113.7 10.0.0.1 51234 443\r\n",
        ] {
            assert_eq!(
                parse(invalid),
                Parsed::Invalid,
                "{}",
                String::from_utf8_lossy(invalid)
            );
        }
        // Longer than a v1 header can be (107 bytes), without its end
        let long = [b"PROXY TCP4 ".as_slice(), &[b'1'; 120]].concat();
        assert_eq!(parse(&long), Parsed::Invalid);
    }

    #[test]
    fn test_v2_headers() {
        let ipv4 = [203, 0, 113, 7, 10, 0, 0, 1, 0xc8, 0x22, 0x01, 0xbb];
        let bytes = v2(1, 0x11, &ipv4, b"GET /");
        assert_eq!(parse(&bytes), header(28, "203.0.113.7:51234"));

        let mut ipv6 = vec![0x20, 0x01, 0x0d, 0xb8];
        ipv6.extend_from_slice(&[0; 11]);
        ipv6.push(7);
        ipv6.extend_from_slice(&[0; 16]);
        ipv6.extend_from_slice(&[0xc8, 0x22, 0x01, 0xbb]);
        let bytes = v2(1, 0x21, &ipv6, b"");
        assert_eq!(parse(&bytes), header(52, "[2001:db8::7]:51234"));

        // With TLVs after the addresses: skipped with them
        let tlvs = [ipv4.as_slice(), &[0x04, 0x00, 0x01, 0x00]].concat();
        let bytes = v2(1, 0x11, &tlvs, b"rest");
        assert_eq!(parse(&bytes), header(32, "203.0.113.7:51234"));

        // The load balancer's own (LOCAL), or another protocol than TCP: no client
        for (command, family, addresses) in [
            (0, 0x00, &[][..]),
            (0, 0x11, &ipv4[..]),
            (1, 0x12, &ipv4[..]),
            (1, 0x00, &[][..]),
        ] {
            let bytes = v2(command, family, addresses, b"");
            assert_eq!(
                parse(&bytes),
                Parsed::Header {
                    len: 16 + addresses.len(),
                    source: None
                },
                "command {command}, family {family:#x}"
            );
        }
    }

    #[test]
    fn test_v2_headers_arriving_in_pieces() {
        let ipv4 = [203, 0, 113, 7, 10, 0, 0, 1, 0xc8, 0x22, 0x01, 0xbb];
        let bytes = v2(1, 0x11, &ipv4, b"");
        for end in 0..bytes.len() {
            assert_eq!(parse(&bytes[..end]), Parsed::Incomplete, "{end} bytes");
        }
    }

    #[test]
    fn test_invalid_v2_headers() {
        let ipv4 = [203, 0, 113, 7, 10, 0, 0, 1, 0xc8, 0x22, 0x01, 0xbb];
        // Another version than 2, another command than LOCAL and PROXY
        let mut bytes = v2(1, 0x11, &ipv4, b"");
        bytes[12] = 0x11;
        assert_eq!(parse(&bytes), Parsed::Invalid);
        assert_eq!(parse(&v2(2, 0x11, &ipv4, b"")), Parsed::Invalid);
        // Addresses shorter than their family's
        assert_eq!(parse(&v2(1, 0x11, &ipv4[..8], b"")), Parsed::Invalid);
        assert_eq!(parse(&v2(1, 0x21, &ipv4, b"")), Parsed::Invalid);
    }

    #[test]
    fn test_parsing_allocates_nothing() {
        let ipv4 = [203, 0, 113, 7, 10, 0, 0, 1, 0xc8, 0x22, 0x01, 0xbb];
        let v2 = v2(1, 0x11, &ipv4, b"");
        crate::allocations::assert_no_allocations("parsing PROXY headers", || {
            parse(b"PROXY TCP6 2001:db8::7 2001:db8::1 51234 443\r\n");
            parse(&v2);
            parse(b"GET / HTTP/1.1\r\n");
        });
    }
}
