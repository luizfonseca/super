# Changelog

## [0.5.0](https://github.com/luizfonseca/super/compare/super-v0.4.0...super-v0.5.0) (2026-10-27)


### Features

* upstreams over TLS on any port, and their certificates checked ([ce32e42](https://github.com/luizfonseca/super/commit/ce32e42d31a903568e6cec306e8c1b9091ac346c))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * super_admin_ui bumped from 0.4.0 to 0.5.0
    * super_plugins bumped from 0.4.0 to 0.5.0
    * super_core bumped from 0.4.0 to 0.5.0
    * super_store bumped from 0.4.0 to 0.5.0
  * dev-dependencies
    * super_plugins bumped from 0.4.0 to 0.5.0
    * super_store bumped from 0.4.0 to 0.5.0

## [0.4.0](https://github.com/luizfonseca/super/compare/super-v0.3.2...super-v0.4.0) (2026-10-27)


### Features

* config.hcl, read before Proksi's proksi.hcl ([de55638](https://github.com/luizfonseca/super/commit/de5563849fc91c4b3aead01e1a765e91c2fc1af2))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * super_admin_ui bumped from 0.3.2 to 0.4.0
    * super_plugins bumped from 0.3.2 to 0.4.0
    * super_core bumped from 0.3.2 to 0.4.0
    * super_store bumped from 0.3.2 to 0.4.0
  * dev-dependencies
    * super_plugins bumped from 0.3.2 to 0.4.0
    * super_store bumped from 0.3.2 to 0.4.0

## [0.3.2](https://github.com/luizfonseca/super/compare/super-v0.3.1...super-v0.3.2) (2026-10-26)


### Bug Fixes

* proxy and upstream times in access logs and the admin ([422702b](https://github.com/luizfonseca/super/commit/422702b7839f1919b42787043977b25561ceeaab))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * super_admin_ui bumped from 0.3.1 to 0.3.2
    * super_plugins bumped from 0.3.1 to 0.3.2
    * super_core bumped from 0.3.1 to 0.3.2
    * super_store bumped from 0.3.1 to 0.3.2
  * dev-dependencies
    * super_plugins bumped from 0.3.1 to 0.3.2
    * super_store bumped from 0.3.1 to 0.3.2

## [0.3.1](https://github.com/luizfonseca/super/compare/super-v0.3.0...super-v0.3.1) (2026-10-26)


### Bug Fixes

* supervise only command lines that serve ([5e3fb21](https://github.com/luizfonseca/super/commit/5e3fb21186968a5ecfa6c8fbadca9c25434276d0))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * super_admin_ui bumped from 0.3.0 to 0.3.1
    * super_plugins bumped from 0.3.0 to 0.3.1
    * super_core bumped from 0.3.0 to 0.3.1
    * super_store bumped from 0.3.0 to 0.3.1
  * dev-dependencies
    * super_plugins bumped from 0.3.0 to 0.3.1
    * super_store bumped from 0.3.0 to 0.3.1

## [0.3.0](https://github.com/luizfonseca/super/compare/super-v0.2.0...super-v0.3.0) (2026-10-26)


### Features

* **core:** websockets over HTTP/2 (RFC 8441) ([64b668e](https://github.com/luizfonseca/super/commit/64b668ee371d93186d1e28fdab54bc4a003e472d))
* HTTP/3 on the HTTPS addresses (server.http3) ([15b30fa](https://github.com/luizfonseca/super/commit/15b30fada2cca85ca38e978a74ebb9ad33f4cff4))


### Bug Fixes

* pedantic lints of the HTTP/3 code ([013f83d](https://github.com/luizfonseca/super/commit/013f83dff9e579e7237739088c993673e5ccebd8))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * super_admin_ui bumped from 0.2.0 to 0.3.0
    * super_plugins bumped from 0.2.0 to 0.3.0
    * super_core bumped from 0.2.0 to 0.3.0
    * super_store bumped from 0.2.0 to 0.3.0
  * dev-dependencies
    * super_plugins bumped from 0.2.0 to 0.3.0
    * super_store bumped from 0.2.0 to 0.3.0

## [0.2.0](https://github.com/luizfonseca/super/compare/super-v0.1.0...super-v0.2.0) (2026-10-26)


### Features

* **core:** stale-while-revalidate, revalidation, and 304s and ranges from the cache ([a65f063](https://github.com/luizfonseca/super/commit/a65f063c624679a068949fa390c28db58dc437eb))
* **core:** work_stealing = false, as Pingora's ([790788c](https://github.com/luizfonseca/super/commit/790788c92cda5c38901508c487c2304f9b30b8ec))
* mimalloc, as an option (feature mimalloc) ([8d94fc7](https://github.com/luizfonseca/super/commit/8d94fc7f66cdb5b6ea82dd76f3687d6cdf6223ac))
* Proksi, on the hyper core ([dd80f1f](https://github.com/luizfonseca/super/commit/dd80f1f9a4bdf1c7e56f0834742382351ab04514))


### Bug Fixes

* **core:** upgrades and shutdowns as Pingora's ([fab8626](https://github.com/luizfonseca/super/commit/fab86261cb2d3abbf36ce90e564a59c5b7ff98ec))


### Performance Improvements

* **core:** threads borrow each other's idle connections; work_stealing off by default ([7732056](https://github.com/luizfonseca/super/commit/7732056f817eebcf59ae0bce610dd82663805043))
* the kernel sizes upstream connections' receive buffers ([6221eb9](https://github.com/luizfonseca/super/commit/6221eb90da77231f40dade1f27842c3e62594bd1))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * super_admin_ui bumped from 0.1.0 to 0.2.0
    * super_plugins bumped from 0.1.0 to 0.2.0
    * super_core bumped from 0.1.0 to 0.2.0
    * super_store bumped from 0.1.0 to 0.2.0
  * dev-dependencies
    * super_plugins bumped from 0.1.0 to 0.2.0
    * super_store bumped from 0.1.0 to 0.2.0

## Changelog
