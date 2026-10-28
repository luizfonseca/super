# Changelog

## [0.6.2](https://github.com/luizfonseca/super/compare/super_core-v0.6.1...super_core-v0.6.2) (2026-10-28)


### Performance Improvements

* response headers by value, and each backend's peer built once and shared by its requests ([5ddea19](https://github.com/luizfonseca/super/commit/5ddea19a38b91105f56dc24c16399391c341507a))

## [0.6.1](https://github.com/luizfonseca/super/compare/super_core-v0.6.0...super_core-v0.6.1) (2026-10-28)


### Bug Fixes

* the coarse clock's lint on Linux ([7e03a80](https://github.com/luizfonseca/super/commit/7e03a8036c451f95b34bc05ffaaf3357cbe2edb9))


### Performance Improvements

* an upstream's request has room for the headers the phases add ([5ef34c2](https://github.com/luizfonseca/super/commit/5ef34c25927a106e3503cdef4c9bd9e8645239ff))
* forwarded headers shared from the connection and the request, and no names built for Connection: keep-alive ([4076771](https://github.com/luizfonseca/super/commit/407677141e32efb7fe1638492c4576bdc83033ae))
* header names in Title-Case, unless server.preserve_header_case keeps them as written ([9017540](https://github.com/luizfonseca/super/commit/90175404010cf651cb8fc4fb29db5f7213c4df32))
* only the connection headers there are removed, after one look at the names ([5cef22e](https://github.com/luizfonseca/super/commit/5cef22e0286896de5c81a5b366360fd64b0aeb2b))
* the upstream pool reads a coarse clock for idle connections ([15ab5ee](https://github.com/luizfonseca/super/commit/15ab5ee3473d7b3a800ff28e15820eab8bbf7adc))

## [0.6.0](https://github.com/luizfonseca/super/compare/super_core-v0.5.0...super_core-v0.6.0) (2026-10-28)


### Bug Fixes

* Host, absolute-form targets, cookie crumbs and 204s as HTTP's RFCs say ([04d233a](https://github.com/luizfonseca/super/commit/04d233ad6bdd5c62d252e5b19c7bc5de5d25a9fd))
* HTTP/3 requests answered before their body ends aren't failed for their client ([89c94c4](https://github.com/luizfonseca/super/commit/89c94c44a85985acf8aea0349f52941b0f6f2f47))
* HTTP/3 requests with HTTP/1.1's connection headers are refused as malformed ([b3e591a](https://github.com/luizfonseca/super/commit/b3e591ae3930b12f26020e7f01f4982de3209491))

## [0.5.0](https://github.com/luizfonseca/super/compare/super_core-v0.4.0...super_core-v0.5.0) (2026-10-27)


### Features

* upstreams over TLS on any port, and their certificates checked ([7b4bd41](https://github.com/luizfonseca/super/commit/7b4bd414959e40ab609dc699b42d05cc82620908))


### Bug Fixes

* a rewrite keeps the host of requests to HTTP/2 upstreams ([7f4222d](https://github.com/luizfonseca/super/commit/7f4222d68e457b95b93f5e8a60901fe7cca862d6))
* uploads refused early get their answer, not a reset connection ([936ffd6](https://github.com/luizfonseca/super/commit/936ffd69a6d08d06fc682823e58be769e9eb05e1))

## [0.4.0](https://github.com/luizfonseca/super/compare/super_core-v0.3.2...super_core-v0.4.0) (2026-10-27)


### Bug Fixes

* an upstream's Host reaches HTTP/2 upstreams ([e3e10e7](https://github.com/luizfonseca/super/commit/e3e10e7cfd612f95374dc575a1c97d3410e8ecf4))

## [0.3.2](https://github.com/luizfonseca/super/compare/super_core-v0.3.1...super_core-v0.3.2) (2026-10-26)


### Miscellaneous Chores

* **super_core:** Synchronize super versions

## [0.3.1](https://github.com/luizfonseca/super/compare/super_core-v0.3.0...super_core-v0.3.1) (2026-10-26)


### Miscellaneous Chores

* **super_core:** Synchronize super versions

## [0.3.0](https://github.com/luizfonseca/super/compare/super_core-v0.2.0...super_core-v0.3.0) (2026-10-26)


### Features

* **core:** HTTP/3 (QUIC) service ([32bd8f0](https://github.com/luizfonseca/super/commit/32bd8f0ddb4c7254ba8063bda88e96210a684d47))
* **core:** websockets over HTTP/2 (RFC 8441) ([b496a92](https://github.com/luizfonseca/super/commit/b496a92af1e91c3162e39cb20fb9b71caf8fddd8))

## [0.2.0](https://github.com/luizfonseca/super/compare/super_core-v0.1.0...super_core-v0.2.0) (2026-10-26)


### Features

* **core:** connections and their TLS handshakes ([245f63d](https://github.com/luizfonseca/super/commit/245f63d44821ed0687a554d1963355ea1412f215))
* **core:** connections to upstreams, kept for the next requests ([7900374](https://github.com/luizfonseca/super/commit/79003743155796f7d09d719836d6b589baa9adc2))
* **core:** errors and headers of the request phases ([f7e67a5](https://github.com/luizfonseca/super/commit/f7e67a5f6b2cc9949f9125d1173c1832f20a6818))
* **core:** load balancers, with their discovery and health checks ([5931ef5](https://github.com/luizfonseca/super/commit/5931ef5e04f2f2d11c67dec782b9339e99cd70d0))
* **core:** Pingora's request phases on hyper, with the cache and compression ([6f6de15](https://github.com/luizfonseca/super/commit/6f6de1516da168b8261e28a0d5dc44d606f10598))
* **core:** stale-while-revalidate, revalidation, and 304s and ranges from the cache ([2c4c13b](https://github.com/luizfonseca/super/commit/2c4c13bac9b5250e15252dd04354fdd876d7ef3d))
* **core:** TCP Fast Open to upstreams (Linux) ([a1a2e7e](https://github.com/luizfonseca/super/commit/a1a2e7e69a745d2f593a634eb650a1a76a42060d))
* **core:** the server, its services, and HTTP sessions on hyper ([df93366](https://github.com/luizfonseca/super/commit/df933667728697daf6c8b34481f8ed9bc2469e61))
* **core:** work_stealing = false, as Pingora's ([7ecfe50](https://github.com/luizfonseca/super/commit/7ecfe500c5b7f155a161332d29a380b86658a5d4))
* **plugins:** Proksi's plugins, on the core's sessions ([7b894f0](https://github.com/luizfonseca/super/commit/7b894f08f1ce6f97feb46617fe02751ad597b041))
* Proksi, on the hyper core ([622fedc](https://github.com/luizfonseca/super/commit/622fedcc977d90c83dbacfde38da25121c17c632))


### Bug Fixes

* **core:** compatible with Proksi on headers, gRPC, HEAD caching and error logs; balanced threads ([48da7b0](https://github.com/luizfonseca/super/commit/48da7b0b484127150c7eca82b99782456dc4cde1))
* **core:** upgrades and shutdowns as Pingora's ([663cf7c](https://github.com/luizfonseca/super/commit/663cf7c26c69d4b10037dfb05b4e961a1e0f591b))
* **core:** uploads the upstream answers before it took the whole body ([11fb1ac](https://github.com/luizfonseca/super/commit/11fb1ace234177c513d4c18ae07b3ea9e6f90266))


### Performance Improvements

* **core:** each thread accepts the connections it serves ([ab42a93](https://github.com/luizfonseca/super/commit/ab42a9335b1ee76a25207653f78c1638d8353df4))
* **core:** threads borrow each other's idle connections; work_stealing off by default ([7b3329f](https://github.com/luizfonseca/super/commit/7b3329fff9195b1d010ca31f4b8855572dff18a0))
* **core:** threads that share nothing on the hot path ([b70050b](https://github.com/luizfonseca/super/commit/b70050b4f7e49064a9f19212eda0be9ea6cf1159))
* **core:** timers only when waiting; borrowing only for threads seldom sending to an upstream ([37093d2](https://github.com/luizfonseca/super/commit/37093d20892e990a09b3bc744746ddff7122ac8b))

## Changelog
