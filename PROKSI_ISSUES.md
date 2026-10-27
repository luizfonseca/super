# Issues found in Proksi while building Super

Found while porting Proksi to hyper; each one is (or would be) fixed in Super. Paths are in
Proksi's repository. **Confirmed** means checked in Proksi's code or measured; **to verify**
depends on how Pingora behaves, which wasn't checked.

## Performance

### Upstream connections read bodies 8 KiB at a time — confirmed

`crates/proksi/src/proxy_server/mod.rs` (`default_peer_opts`): `po.tcp_recv_buf = Some(1024 * 8)`
fixes each upstream socket's receive buffer at 8 KiB, which turns off the kernel's tuning of it.
Every response body is read 8 KiB at a time, with a wakeup for each read.

Measured in Super (same setting, then removed), 4 threads, 1 MB downloads: 4.6 → 19.5 GB/s, and
869 → 148 µs of CPU per MB; 10 MB downloads: 2.6 → 21.8 GB/s. Proksi measured 2.6 and 2.4 GB/s in
the same benchmark, so it's capped the same way.

Fix: `po.tcp_recv_buf = None` (about 14 MB more memory under that load, from larger buffers).

### Threads share one pool of upstream connections — confirmed (Pingora's design)

With 4 threads, Proksi uses about twice the CPU per request it uses with one (41.6 µs instead of
20.3; `docs/general/performance.mdx` says as much). Most of it is threads contending for the
pool and waking each other. Not a bug, but `work_stealing = false` only helps a little there
(35.6 µs); Super keeps connections per thread (borrowing another thread's only for upstreams a
thread seldom uses) and stays at about 13 µs with 4 threads.

## Cache

### Responses of any status are cached — to verify

`crates/proksi/src/proxy_server/https_proxy.rs` (`response_cache_filter`): the comment says "Only
cache GET and HEAD requests with 2xx responses", but only the method is checked. Unless Pingora
refuses other statuses itself, a `404`, a `500` or a `206` is stored and served until it expires.

Fix: check `resp.status.is_success() && resp.status != 206` too.

### A `HEAD` hit can store an empty body in memory — to verify

`crates/proksi/src/cache/disk/handlers.rs` (`DiskCacheHitHandler::finish`) copies what was read
of the body to the memory cache. If Pingora calls `finish` after a `HEAD` hit (whose body isn't
read), it stores an empty body (unless the memory copy had the same length), and the next `GET`
served from memory gets an empty body. The same goes for anything that reads part of a body.

Fix: copy only once the whole body was read (count the bytes against the metadata's length).

### `HEAD` responses can be cached, empty — to verify

`crates/proksi/src/proxy_server/https_proxy.rs`: `CACHEABLE_METHODS` holds `HEAD`, so a `HEAD`
that misses the cache can store its response, which has no body, under the same key as a `GET`
(`cache_key_callback` doesn't include the method). Unless Pingora fetches a `HEAD` miss as a
`GET`, the next `GET` gets an empty body.

Fix: only `GET` responses are stored (a `HEAD` can still be served from them).

### Renewed metadata isn't seen by the memory copy — confirmed

`crates/proksi/src/cache/disk/storage.rs` (`update_meta`) rewrites the metadata file, but
`lookup` reads the memory copy first, which keeps the old metadata: a response revalidated with
a `304` still looks stale until the memory copy is evicted.

Fix: replace the memory copy's metadata too (as Super does).

### Purging does nothing — confirmed

`crates/proksi/src/cache/disk/storage.rs` (`purge`) logs and answers `Purged` without removing
anything.

Fix: remove the `.cache` and `.metadata` files and the memory copy (if purging is meant to work).

### `--version` reports a stopped worker — confirmed

`crates/proksi/src/supervisor.rs` (`should_supervise`): on Linux, every command line starts a
supervisor, including those that don't serve. `proksi --version` (or `--help`, `check`) runs in
a worker, and the supervisor then prints `proksi supervisor: worker 6 stopped (exit code 0)`
after the version.

Fix: supervise only command lines that serve: read them with `Config::try_parse_from` (no
configuration files), and supervise when that succeeds without a subcommand (as Super does).

### Access logs can't tell the proxy's time from the upstream's — improvement

`crates/proksi/src/proxy_server/https_proxy.rs` (`logging`): `duration_ms` runs from
`request_filter` to `logging`, so it's the whole request (plugins, the upstream, the response
body's transfer). Nothing says how much of it was proksi's own (routing, rules, `forward_auth`
and other plugins) or the upstream's: the upstream's time is only in the metrics' percentiles.

Fix (as Super does): keep when the first attempt at an upstream starts (`upstream_peer`) and when
its response headers come (`upstream_response_filter`), and log `proxy_ms` (until the first
attempt, or all of it without an upstream) and `upstream_ms` (until the response headers,
attempts and connecting included), with microsecond precision. The same two fields in the
requests stream (`/api/v1/requests/stream`), and in the admin UI's request log (a `Proxy`
column, both times in a request's details). The UI's `ms()` shows times under 1 ms in µs.

### Upstreams' certificates are never checked, and TLS is only for port 443 — security

`crates/proksi/src/proxy_server/mod.rs` (`default_peer_opts`): `verify_cert = false` for every
upstream, so whoever can intercept the connection to an HTTPS upstream (an external service, over
the internet) can pose as it. And `https_proxy.rs` (`upstream_peer`) and `routing/mirror.rs` reach
an upstream over TLS only on port 443: an HTTPS upstream on `8443` can't be reached at all.

Fix (as Super does): upstream settings `tls` (TLS on any port; port 443 by default) and `verify`
(check the certificate for `sni`; off by default, so existing configurations keep working, and
refused without `sni` or TLS). Pingora's connector looks for certificate authorities where the
vendored OpenSSL was built, not where the system has them: load the system's (`openssl-probe`)
or `verify` fails for every certificate.

### Uploads refused early can reach clients as a reset connection — to verify

When a request is answered before its body is read (`body_limit`'s `413`, `basic_auth`'s `401`),
Super's HTTP/1.1 connections were closed while the client still sent the body: Linux then resets
the connection, and clients that write their body before reading (hyper, so reqwest; browsers
often) fail with a broken pipe instead of reading the answer. Super's test of it:
`test_uploads_refused_early_are_answered` (it only fails on Linux). Check whether Pingora's
HTTP/1.1 server lingers.

Fix (as Super does, and nginx's lingering close): once its side of the connection is shut down,
keep reading and discarding what the client sends, for 10s at most and 2s without anything, then
close. Not for upgraded connections (tunnels), whose bytes are the upstream's.

### What HTTP's RFCs require of a proxy — to verify

Super's table of RFC 9110, 9112, 9113 and 9114's requirements on a proxy
(`crates/super/tests/spec.rs`) failed these before Super was fixed. Proksi may well fail them
too (they're in what Pingora hands it, or in what Proksi's phases add):

- RFC 9113 §8.2.3 / RFC 9114 §4.2.1: an HTTP/2 client's `Cookie` fields (crumbs) are sent to an
  HTTP/1.1 upstream as several lines: they MUST be joined by `; `. Many apps read the first only.
- RFC 9112 §3.2: a request with two `Host` fields MUST get a `400` (it was proxied).
- RFC 9112 §3.2.2: an absolute-form target's host MUST be used, not `Host`'s.
- RFC 9110 §8.6: a `204` MUST NOT have `Content-Length` (an upstream's went on to HTTP/2 clients).
- RFC 9110 §7.6.2: `Max-Forwards` (an `OPTIONS` or `TRACE` at `0` is answered, not forwarded).
- RFC 9110 §7.6.3: a gateway MUST add `Via` to the requests it forwards.

Run the table against Proksi (`PROKSI_BIN` makes the harness start another binary) to see which.
