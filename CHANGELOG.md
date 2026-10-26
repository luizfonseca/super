# Changelog

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
