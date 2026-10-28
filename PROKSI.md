# Super

Proksi, rebuilt on [hyper](https://hyper.rs) instead of Pingora: a case study of what a hyper
core looks like, and how it performs.

Super is interchangeable with Proksi: the same configuration (files, `PROKSI_*` environment
variables, `/etc/proksi` paths), the same plugins, the same stores (their tables and keys, so
replicas of both can share one), the same cache keys and the same metrics. Only Pingora is
replaced, by `super_core`, which keeps Pingora's request phases (`ProxyHttp`) on hyper.

The binary is `super`. Proksi's guide (the [README](./README.md)) applies as it is. Bugs found in
Proksi while porting it are in [`PROKSI_ISSUES.md`](./PROKSI_ISSUES.md).

## Layout

- `crates/super_core`: the core on hyper, with Pingora's request phases (`ProxyHttp`), its
  server and services, connections to upstreams, load balancers, cache and compression
- `crates/super`: Proksi itself (package `super_proxy`, binary `super`)
- `crates/super_plugins`, `crates/super_store`, `crates/super_admin_ui`: Proksi's plugins,
  stores and admin UI

## How it differs from Proksi

Proksi's tests (unit and end-to-end, on Linux and macOS) pass as they are. What still differs:

- **Where Super looks**: without `-c`, in `/etc/superconfig`, then Proksi's `/etc/proksi` (and
  `/etc/proksi/configs`, where Proksi looks); certificates go to `/etc/superconfig/letsencrypt`,
  unless only Proksi's `/etc/proksi/letsencrypt` has some.
- **Configuration files**: in a directory, Super reads `config.hcl` (or `config.yml`,
  `config.yaml`), and Proksi's `proksi.hcl` (or `proksi.yml`, `proksi.yaml`) when there's no
  `config.*` file. Proksi only reads `proksi.*`: keep that name for a directory both read.
- **Upstreams over TLS**: `tls = true` reaches an upstream over TLS on any port (Proksi only
  does on port 443), and `verify = true` checks its certificate (Proksi never does). Super
  also trusts the system's certificate authorities. Proksi refuses both settings.
- **What HTTP's RFCs require of a proxy** (Super's own table, `crates/super/tests/spec.rs`):
  requests to upstreams carry `Via` (`1.1 super`, `2 super`...); an `OPTIONS` or `TRACE` with
  `Max-Forwards: 0` is answered by Super, and `Max-Forwards` is decremented otherwise; a request
  with two `Host` fields gets a `400`; an HTTP/1.1 absolute-form target (`GET http://a.test/`)
  is routed by its host, not `Host`'s; HTTP/2 and HTTP/3 `Cookie` fields reach upstreams joined
  in one; and a `204` has no `Content-Length`.
- **Header names** of HTTP/1.1 requests and responses are Title-Case (`Content-Type`), where
  Proksi keeps them as clients and upstreams wrote them: HTTP compares names without case, and
  keeping it costs every request. `server { preserve_header_case = true }` keeps them as
  written (then the ones Super adds are Title-Case). HTTP/2 is lowercase in both.
- **Stale responses** in the cache: within `stale_while_revalidate`, the first request after a
  response expires revalidates it with the upstream while the others are served it (Pingora
  revalidates in the background, and serves every request); they're served when the upstream
  fails within `stale_if_error`.
- **Hop-by-hop request headers** (`Connection`, `Keep-Alive`, `TE` but `TE: trailers`...) aren't
  sent to upstreams, as RFC 9110 says (Pingora sends most of them to HTTP/1.1 upstreams).
- **Logs**: errors read as Pingora's (`Fail to proxy: ..., status: 502, tries: 1, ...`), but
  their target is `super_core::...` instead of `pingora_...`.
- **An upstream that answers before reading a request body, then closes** may lose its answer
  (502) when the close resets the connection before hyper's client reads it.
- **`daemon = true`** writes its pid to `/tmp/super.pid` (Pingora's `/tmp/pingora.pid`).
- **Upgrades** hand the listening sockets over the same way between Super workers, but not from a
  Proksi worker to a Super one (the socket is `super-upgrade-*.sock`): replace Proksi with Super
  by restarting, or behind a load balancer.
- **Certificates** are chosen with the name read from the client's `ClientHello`, before the
  handshake, instead of an asynchronous OpenSSL callback (the same certificates are served).
- **`server.work_stealing`** is off unless set (Proksi's is on): each connection stays on the
  thread that accepted it, which costs far less CPU per request with the core. Set it to `true`
  when Super gets fewer CPUs than it has threads.
- **Upstream connections** are kept per thread, and a thread that never kept one to an upstream
  borrows another thread's idle connection (as Pingora's shared pool would): as fast as pools of
  their own, with more connections kept open than Proksi's single pool when every thread sends
  requests to every upstream.
- **Websockets over HTTP/2** (RFC 8441): HTTP/2 clients (browsers) can open websockets on their
  connection, as an extended `CONNECT`; the phases and the upstream see it as the HTTP/1.1
  upgrade it stands for (`GET` with `Upgrade: websocket`). Upgrades always go to upstreams over
  HTTP/1.1, even those that speak HTTP/2. Proksi's HTTP/2 clients open a separate HTTP/1.1
  connection for each websocket.
- **HTTP/3** (`server.http3 = true`, off by default; Proksi ignores the option): QUIC on the
  HTTPS addresses' UDP ports, through the same phases and plugins, and an `Alt-Svc` header on
  HTTPS responses so browsers find it. Publish the UDP port too (`-p 443:443/udp`), or leave it
  off: browsers that are told about HTTP/3 and can't reach it try it, then fall back. It does
  nothing behind a CDN (the CDN speaks HTTP/3 to visitors, and TCP to Super). HTTP/3 uses the
  certificates HTTPS handshakes read (a browser connects over TCP first), read again from the
  store every minute. An upgrade (`--upgrade`) doesn't hand the UDP socket over:
  the new process answers HTTP/3 once the old one has stopped.

## Benchmarks

Pull requests are benchmarked against `main`, as in Proksi.

As on Proksi's performance page: nginx, Proksi and Super proxying to the same nginx upstream,
with the same certificate (RSA 2048), access logs off, keep-alive to the upstream, and `oha`
with 50 connections on the same machine (Linux arm64, 8 CPUs, nginx 1.26.3, medians of 3
alternating rounds). Proksi and Super run with their default `work_stealing`; memory is the
proxy's private memory (without its executable's pages).

**1 thread**

| | Requests/s (HTTP / HTTPS) | CPU per request (HTTP / HTTPS) | Latency p99 (HTTP) | Memory (idle → under load) |
| --- | ---: | ---: | ---: | ---: |
| nginx | 110k / 82k | 9.1 / 12.1 µs | 0.67 ms | 7 → 10 MB |
| Proksi | 50k / 41k | 20.1 / 24.6 µs | 1.54 ms | 10 → 17 MB |
| **Super** | 72k / 61k | 13.8 / 16.3 µs | 1.09 ms | 6 → 12 MB |

**4 threads**

| | Requests/s (HTTP / HTTPS) | CPU per request (HTTP / HTTPS) | Latency p99 (HTTP) | Memory (idle → under load) |
| --- | ---: | ---: | ---: | ---: |
| nginx | 393k / 333k | 8.5 / 10.7 µs | 0.39 ms | 22 → 26 MB |
| Proksi | 85k / 80k | 40.1 / 43.5 µs | 1.06 ms | 10 → 20 MB |
| **Super** | 294k / 252k | 12.4 / 15.0 µs | 0.41 ms | 6 → 14 MB |

nginx and Super are from the latest run; Proksi's rows from an earlier one on the same setup (nginx was within 5% of this one there).

**Other workloads** (4 threads)

| | nginx | Proksi | Super |
| --- | ---: | ---: | ---: |
| A new connection for each request (HTTP) | 193k req/s | 63k req/s | 140k req/s |
| A new connection for each request (TLS) | 20k req/s | 14k req/s | 16k req/s |
| 1 MB downloads | 10.4 GB/s | 2.6 GB/s | 19.4 GB/s |
| 10 MB downloads | 7.8 GB/s | 2.4 GB/s | 21.0 GB/s |
| 1 MB uploads | 7.0 GB/s | 13.9 GB/s | 23.0 GB/s |
| HTTP/2, 10 connections × 10 streams | 409k req/s | 99k req/s | 336k req/s |

With TLS, most of a new connection's cost is OpenSSL's handshake (RSA, and the post-quantum key
exchange Proksi and Super build with). Proksi reads bodies from upstreams 8 KiB at a time (see
`PROKSI_ISSUES.md`).

These come from a single machine, where the proxy, the upstream and the load generator share
the same CPUs: a comparison, not the throughput of production.
