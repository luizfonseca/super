#!/usr/bin/env bash
# Runs conformance suites against Super, and checks their results against what's expected
# (`expected/<suite>.txt`): `conformance/run.sh [h2spec] [h3spec]` (both without any).
#
# The suites' binaries are downloaded once (pinned, checked) to `conformance/.tools`. Super is
# built unless `SUPER_BIN` names one. Results go to `conformance/results/<suite>.json`; the
# README's table is written from them with `conformance/report.py readme`.
set -euo pipefail

cd "$(dirname "$0")/.."
root=conformance
suites=("$@")
[ ${#suites[@]} -eq 0 ] && suites=(h2spec h3spec)
port=${CONFORMANCE_PORT:-19443}
tools="$root/.tools"
work=$(mktemp -d)
mkdir -p "$tools" "$root/results"

# The pinned binaries of each suite, by system: name, URL, SHA-256
H2SPEC_VERSION=2.6.0
H3SPEC_VERSION=0.1.14
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)
    h2spec_archive=h2spec_linux_amd64.tar.gz
    h2spec_sha=157ee0de702e01ad40e752dbf074b366027e550c8e7504f9450da2809e279318
    h3spec_binary=h3spec-linux-x86_64
    h3spec_sha=7f2e3232715a3f6ff5212d408b2a3e74d6aa3ca06f96efca9a4e5dc55f3c6860
    ;;
  Darwin-*)
    h2spec_archive=h2spec_darwin_amd64.tar.gz
    h2spec_sha=981cb9f90a6f5e36300063022bd4eb7438d3dcf66d63a146a8541359697d1601
    h3spec_binary=h3spec-mac-arm64
    h3spec_sha=1d76372c74b5fbc28256229e01135f7c878045b8b0a1dc578eaa866e10df140a
    ;;
  *)
    echo "no conformance tools for $(uname -s) $(uname -m)" >&2
    exit 1
    ;;
esac

# Downloads `url` to `path` once, checking its SHA-256
fetch() {
  local url=$1 path=$2 sha=$3
  [ -f "$path" ] && return
  curl -fsSL "$url" -o "$path.part"
  echo "$sha  $path.part" | shasum -a 256 -c - >/dev/null
  mv "$path.part" "$path"
}

h2spec() {
  local archive="$tools/$h2spec_archive"
  fetch "https://github.com/summerwind/h2spec/releases/download/v$H2SPEC_VERSION/$h2spec_archive" "$archive" "$h2spec_sha"
  [ -x "$tools/h2spec" ] || tar -xzf "$archive" -C "$tools" h2spec
  # Strict: the SHOULDs too
  "$tools/h2spec" --tls --insecure --strict --host localhost --port "$port" \
    --junit-report "$work/h2spec.xml" >"$work/h2spec.out" 2>&1 || true
  python3 "$root/report.py" h2spec "$work/h2spec.xml" "$H2SPEC_VERSION"
}

h3spec() {
  local binary="$tools/$h3spec_binary"
  fetch "https://github.com/kazu-yamamoto/h3spec/releases/download/v$H3SPEC_VERSION/$h3spec_binary" "$binary" "$h3spec_sha"
  chmod +x "$binary"
  "$binary" localhost "$port" --no-validate >"$work/h3spec.out" 2>&1 || true
  python3 "$root/report.py" h3spec "$work/h3spec.out" "$H3SPEC_VERSION"
}

# Super: HTTPS and HTTP/3 on `localhost`, IPv4 and IPv6, with a certificate of its own
if [ -z "${SUPER_BIN:-}" ]; then
  cargo build -q -p super_proxy
  SUPER_BIN=target/debug/super
fi
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 1 \
  -subj /CN=localhost -addext subjectAltName=DNS:localhost \
  -keyout "$work/key.pem" -out "$work/cert.pem" 2>/dev/null
CONFORMANCE_CERT="$work/cert.pem" CONFORMANCE_KEY="$work/key.pem" \
  PROKSI_SERVER__HTTPS_ADDRESS="[::]:$port" PROKSI_SERVER__HTTP_ADDRESS="[::]:$((port + 1))" \
  "$SUPER_BIN" -c "$root/super.hcl" >"$work/super.log" 2>&1 &
super=$!
trap 'kill $super 2>/dev/null; rm -rf "$work"' EXIT

# Ready once it answers over HTTPS, which also readies its certificate for HTTP/3
for _ in $(seq 1 60); do
  curl -sfk "https://localhost:$port/" -o /dev/null && break
  sleep 0.5
done
curl -sfk "https://localhost:$port/" -o /dev/null || {
  echo "Super didn't start:" >&2
  cat "$work/super.log" >&2
  exit 1
}

failed=0
for suite in "${suites[@]}"; do
  echo "== $suite"
  "$suite" || failed=1
done
exit $failed
