# Changelog

## [0.6.0](https://github.com/luizfonseca/super/compare/super-v0.5.0...super-v0.6.0) (2026-10-28)


### Features

* Super's own directory, /etc/superconfig, with Proksi's still read ([3dde9bd](https://github.com/luizfonseca/super/commit/3dde9bd3f30a279ff42210911dd1dcc9081d18a4))


### Bug Fixes

* HTTP/3 requests answered before their body ends aren't failed for their client ([89c94c4](https://github.com/luizfonseca/super/commit/89c94c44a85985acf8aea0349f52941b0f6f2f47))
* Via and Max-Forwards, as RFC 9110 requires of intermediaries ([4c88820](https://github.com/luizfonseca/super/commit/4c88820fb1b2972c2ccaa7a4dfe223d85f53f9ff))
* YAML configurations read without the unmaintained unsafe-libyaml ([b4af9bc](https://github.com/luizfonseca/super/commit/b4af9bccd1d229dfacc5fbb63629dd817d701243))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * super_admin_ui bumped from 0.5.0 to 0.6.0
    * super_plugins bumped from 0.5.0 to 0.6.0
    * super_core bumped from 0.5.0 to 0.6.0
    * super_store bumped from 0.5.0 to 0.6.0
  * dev-dependencies
    * super_plugins bumped from 0.5.0 to 0.6.0
    * super_store bumped from 0.5.0 to 0.6.0

## [0.5.0](https://github.com/luizfonseca/super/compare/super-v0.4.0...super-v0.5.0) (2026-10-27)


### Features

* upstreams over TLS on any port, and their certificates checked ([7b4bd41](https://github.com/luizfonseca/super/commit/7b4bd414959e40ab609dc699b42d05cc82620908))


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

* config.hcl, read before Proksi's proksi.hcl ([9056b22](https://github.com/luizfonseca/super/commit/9056b22b54d200bcc72379466f9bd6cbe643b5ba))


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

* proxy and upstream times in access logs and the admin ([3c08331](https://github.com/luizfonseca/super/commit/3c08331e9a3b37daf610f949044cf2a5a9815629))


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

* supervise only command lines that serve ([3e07d53](https://github.com/luizfonseca/super/commit/3e07d533551117aacf716680278984bdd8f4225d))


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

* **core:** websockets over HTTP/2 (RFC 8441) ([b496a92](https://github.com/luizfonseca/super/commit/b496a92af1e91c3162e39cb20fb9b71caf8fddd8))
* HTTP/3 on the HTTPS addresses (server.http3) ([678d8d6](https://github.com/luizfonseca/super/commit/678d8d6e2de667c8724bd5c1c02a3df588cfc3a5))


### Bug Fixes

* pedantic lints of the HTTP/3 code ([03f1f01](https://github.com/luizfonseca/super/commit/03f1f0119e11f2cbfbe00cd393e0fad30c309aba))


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

* **core:** stale-while-revalidate, revalidation, and 304s and ranges from the cache ([2c4c13b](https://github.com/luizfonseca/super/commit/2c4c13bac9b5250e15252dd04354fdd876d7ef3d))
* **core:** work_stealing = false, as Pingora's ([7ecfe50](https://github.com/luizfonseca/super/commit/7ecfe500c5b7f155a161332d29a380b86658a5d4))
* mimalloc, as an option (feature mimalloc) ([1b59f5a](https://github.com/luizfonseca/super/commit/1b59f5acfc43f0f7be0b6354935c7816e9932ba0))
* Proksi, on the hyper core ([622fedc](https://github.com/luizfonseca/super/commit/622fedcc977d90c83dbacfde38da25121c17c632))


### Bug Fixes

* **core:** upgrades and shutdowns as Pingora's ([663cf7c](https://github.com/luizfonseca/super/commit/663cf7c26c69d4b10037dfb05b4e961a1e0f591b))


### Performance Improvements

* **core:** threads borrow each other's idle connections; work_stealing off by default ([7b3329f](https://github.com/luizfonseca/super/commit/7b3329fff9195b1d010ca31f4b8855572dff18a0))
* the kernel sizes upstream connections' receive buffers ([b28702a](https://github.com/luizfonseca/super/commit/b28702a657490db3c3e57d55368cdc01b1730751))


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
