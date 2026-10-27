# Conformance

The protocols' conformance suites, run against Super. The README's table comes from their last
run here.

| Suite | Checks | Run with |
| --- | --- | --- |
| [h2spec](https://github.com/summerwind/h2spec) | HTTP/2 (RFC 9113) and HPACK (RFC 7541), strict: the SHOULDs too | `conformance/run.sh h2spec` |
| [h3spec](https://github.com/kazu-yamamoto/h3spec) | HTTP/3 (RFC 9114), QPACK (RFC 9204), QUIC (RFC 9000, 9001) | `conformance/run.sh h3spec` |

`make conformance` runs them all and updates the README's table; CI runs `make conformance.check`,
which fails when a case fails that isn't known to, when a known one passes, or when the table is
out of date.

- `super.hcl`: Super as the suites test it, HTTPS and HTTP/3 on `localhost`, every request
  answered by Super itself (the suites test its servers, not its upstreams).
- `run.sh`: downloads the suites' pinned binaries (checked against their SHA-256) to `.tools`,
  starts Super with a certificate of its own, and runs them. `SUPER_BIN` runs another build.
- `report.py`: reads a suite's output into `results/<suite>.json`, checks it against
  `expected/<suite>.txt`, and writes the README's table.
- `expected/<suite>.txt`: the cases known to fail, one a line with why (` # `). A deviation
  Super can't fix (it's in a library) is listed there, never hidden.
