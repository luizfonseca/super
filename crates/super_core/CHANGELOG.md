# Changelog

## [0.3.2](https://github.com/luizfonseca/super/compare/super_core-v0.3.1...super_core-v0.3.2) (2026-10-26)


### Miscellaneous Chores

* **super_core:** Synchronize super versions

## [0.3.1](https://github.com/luizfonseca/super/compare/super_core-v0.3.0...super_core-v0.3.1) (2026-10-26)


### Miscellaneous Chores

* **super_core:** Synchronize super versions

## [0.3.0](https://github.com/luizfonseca/super/compare/super_core-v0.2.0...super_core-v0.3.0) (2026-10-26)


### Features

* **core:** HTTP/3 (QUIC) service ([c4de676](https://github.com/luizfonseca/super/commit/c4de676e23f32104eb39bc580f25f150e442ea08))
* **core:** websockets over HTTP/2 (RFC 8441) ([64b668e](https://github.com/luizfonseca/super/commit/64b668ee371d93186d1e28fdab54bc4a003e472d))

## [0.2.0](https://github.com/luizfonseca/super/compare/super_core-v0.1.0...super_core-v0.2.0) (2026-10-26)


### Features

* **core:** connections and their TLS handshakes ([317aeeb](https://github.com/luizfonseca/super/commit/317aeeba292d0e3db86c5187b324fd9ff5e5eeae))
* **core:** connections to upstreams, kept for the next requests ([0d60170](https://github.com/luizfonseca/super/commit/0d601706817de26344b2be28b6bb5df51db38334))
* **core:** errors and headers of the request phases ([0ba2801](https://github.com/luizfonseca/super/commit/0ba280178d64e85ebd09aaa4e61215890f1aeb46))
* **core:** load balancers, with their discovery and health checks ([557024c](https://github.com/luizfonseca/super/commit/557024c9362708a8da6759be9ce7286fd7cf0159))
* **core:** Pingora's request phases on hyper, with the cache and compression ([6e2bf71](https://github.com/luizfonseca/super/commit/6e2bf7184203e51e4464cf1ad08f81b0b0e567f0))
* **core:** stale-while-revalidate, revalidation, and 304s and ranges from the cache ([a65f063](https://github.com/luizfonseca/super/commit/a65f063c624679a068949fa390c28db58dc437eb))
* **core:** TCP Fast Open to upstreams (Linux) ([f382b65](https://github.com/luizfonseca/super/commit/f382b656aefd03099f3a2c25b6559a66798f9aea))
* **core:** the server, its services, and HTTP sessions on hyper ([9507fe0](https://github.com/luizfonseca/super/commit/9507fe06d14e60bba1a21ee7c1bd86239e1faaea))
* **core:** work_stealing = false, as Pingora's ([790788c](https://github.com/luizfonseca/super/commit/790788c92cda5c38901508c487c2304f9b30b8ec))
* **plugins:** Proksi's plugins, on the core's sessions ([4ad1e34](https://github.com/luizfonseca/super/commit/4ad1e340d1f011237afe81da6a4bedf7923721ff))
* Proksi, on the hyper core ([dd80f1f](https://github.com/luizfonseca/super/commit/dd80f1f9a4bdf1c7e56f0834742382351ab04514))


### Bug Fixes

* **core:** compatible with Proksi on headers, gRPC, HEAD caching and error logs; balanced threads ([2bd001f](https://github.com/luizfonseca/super/commit/2bd001fbc584d0ae13030e8f756705ef06ad21e9))
* **core:** upgrades and shutdowns as Pingora's ([fab8626](https://github.com/luizfonseca/super/commit/fab86261cb2d3abbf36ce90e564a59c5b7ff98ec))
* **core:** uploads the upstream answers before it took the whole body ([28c10b6](https://github.com/luizfonseca/super/commit/28c10b6ab01c6e0ad9dbda41cd36ffe3bfc03cd0))


### Performance Improvements

* **core:** each thread accepts the connections it serves ([d6b507c](https://github.com/luizfonseca/super/commit/d6b507c11cc1791dcd408e4b3cb7ef9b2046afed))
* **core:** threads borrow each other's idle connections; work_stealing off by default ([7732056](https://github.com/luizfonseca/super/commit/7732056f817eebcf59ae0bce610dd82663805043))
* **core:** threads that share nothing on the hot path ([1a4e878](https://github.com/luizfonseca/super/commit/1a4e8784283f60caf96b9621cabe629db860413a))
* **core:** timers only when waiting; borrowing only for threads seldom sending to an upstream ([43e6621](https://github.com/luizfonseca/super/commit/43e6621da953dec830cddd830ebbeca6126761df))

## Changelog
