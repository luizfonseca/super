# Super and Proksi

Super is interchangeable with Proksi: the same configuration, plugins, stores, cache keys and
metrics. This is what still differs between them. Bugs found in Proksi while porting it are in
[`PROKSI_ISSUES.md`](./PROKSI_ISSUES.md).

## How it differs from Proksi

Proksi's tests (unit and end-to-end, on Linux and macOS) pass as they are. What still differs:

- **Header names** of HTTP/1.1 responses: those of upstreams keep their case, as Proksi; those
  Super adds are Title-Case (`X-Request-Id`), where Proksi keeps them as written. HTTP/2 is
  lowercase in both.
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
