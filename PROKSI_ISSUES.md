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
