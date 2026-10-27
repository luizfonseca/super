#!/usr/bin/env python3
"""The results of the conformance suites, and the README's table of them.

    report.py <suite> <output> <version>   results/<suite>.json from a suite's output, checked
                                           against expected/<suite>.txt (Super's own table,
                                           `super`, writes its results itself)
    report.py readme [--check]             the README's table, from results/*.json (--check: is
                                           it up to date?)

`expected/<suite>.txt` lists the cases known to fail, one a line: the case, then ` # ` and why.
A case failing that isn't listed fails the run, and so does one listed that passes, so the list
stays what fails.
"""

import json
import re
import sys
import xml.etree.ElementTree as ElementTree
from pathlib import Path

ROOT = Path(__file__).resolve().parent
README = ROOT.parent / "README.md"
START, END = "<!-- conformance:start -->", "<!-- conformance:end -->"

RFC = "https://www.rfc-editor.org/rfc/rfc{}"

# What each suite checks, as the README says it
SUITES = {
    "h2spec": {
        "spec": f"HTTP/2: [RFC 9113]({RFC.format(9113)}), [RFC 7541]({RFC.format(7541)})",
        "suite": "[h2spec](https://github.com/summerwind/h2spec)",
    },
    "h3spec": {
        "spec": (
            f"HTTP/3: [RFC 9114]({RFC.format(9114)}), [RFC 9204]({RFC.format(9204)}), "
            f"[RFC 9000]({RFC.format(9000)}) and [RFC 9001]({RFC.format(9001)})"
        ),
        "suite": "[h3spec](https://github.com/kazu-yamamoto/h3spec)",
    },
    "super": {
        "spec": (
            f"Proxy: [RFC 9110]({RFC.format(9110)}), [RFC 9112]({RFC.format(9112)}), "
            f"[RFC 9113]({RFC.format(9113)}) and [RFC 9114]({RFC.format(9114)})"
        ),
        "suite": "[super](crates/super/tests/spec.rs)",
    },
    "autobahn": {
        "spec": f"WebSockets: [RFC 6455]({RFC.format(6455)})",
        "suite": "[Autobahn](https://github.com/crossbario/autobahn-testsuite)",
    },
}


def h2spec(output: Path) -> dict:
    """h2spec's JUnit report: each case is its section and title"""
    cases = {"passed": [], "failed": [], "skipped": []}
    for case in ElementTree.parse(output).iter("testcase"):
        name = f"{case.get('package')}: {case.get('classname')}"
        if case.find("failure") is not None or case.find("error") is not None:
            cases["failed"].append(name)
        elif case.find("skipped") is not None:
            cases["skipped"].append(name)
        else:
            cases["passed"].append(name)
    return cases


def h3spec(output: Path) -> dict:
    """h3spec's report: its summary, and each failing case as `  N) <case>`"""
    text = output.read_text()
    summary = re.search(r"(\d+) examples?, (\d+) failures?(?:, (\d+) pending)?", text)
    if summary is None:
        sys.exit(f"h3spec didn't finish:\n{text[-2000:]}")
    total, failures, pending = (int(group or 0) for group in summary.groups())
    failed = sorted(set(re.findall(r"^  \d+\) (.+)$", text, re.MULTILINE)))
    if len(failed) != failures:
        sys.exit(f"h3spec: {failures} failures, {len(failed)} read")
    passed = total - failures - pending
    return {"passed": [None] * passed, "failed": failed, "skipped": [None] * pending}


def autobahn(output: Path) -> dict:
    """Autobahn's report: each case's behaviour, and its closing handshake's (`OK`, `NON-STRICT`
    and `INFORMATIONAL` pass, `UNIMPLEMENTED` is skipped, `FAILED` fails)"""
    cases = {"passed": [], "failed": [], "skipped": []}
    if not output.exists():
        sys.exit("Autobahn didn't write its report: is Docker running?")
    report = json.loads(output.read_text())
    for case, result in report.get("super", {}).items():
        behaviours = (result.get("behavior"), result.get("behaviorClose"))
        if "FAILED" in behaviours:
            cases["failed"].append(case)
        elif "UNIMPLEMENTED" in behaviours:
            cases["skipped"].append(case)
        else:
            cases["passed"].append(case)
    return cases


def expected(suite: str) -> dict:
    """The cases known to fail, and why"""
    known = {}
    path = ROOT / "expected" / f"{suite}.txt"
    lines = path.read_text().splitlines() if path.exists() else []
    for line in lines:
        if not line.strip() or line.startswith("#"):
            continue
        case, _, reason = line.partition(" # ")
        known[case.strip()] = reason.strip()
    return known


def record(suite: str, output: Path, version: str) -> int:
    cases = {"h2spec": h2spec, "h3spec": h3spec, "autobahn": autobahn}[suite](output)
    known = expected(suite)
    failed = set(cases["failed"])
    unexpected = sorted(failed - set(known))
    fixed = sorted(set(known) - failed)
    result = {
        "suite": suite,
        "version": version,
        "passed": len(cases["passed"]),
        "failed": len(failed),
        "skipped": len(cases["skipped"]),
        "known": {case: known[case] for case in sorted(failed & set(known))},
    }
    (ROOT / "results").mkdir(exist_ok=True)
    (ROOT / "results" / f"{suite}.json").write_text(json.dumps(result, indent=2) + "\n")

    run = result["passed"] + result["failed"]
    print(f"{suite} {version}: {result['passed']}/{run} passing, {len(result['known'])} known to fail")
    for case in unexpected:
        print(f"  failing, and not in expected/{suite}.txt: {case}")
    for case in fixed:
        print(f"  passing, but in expected/{suite}.txt (remove it): {case}")
    return 1 if unexpected or fixed else 0


def passing(result: dict) -> str:
    """`passed/run`, with what's known to fail (linked to its list)"""
    run = result["passed"] + result["failed"]
    text = f"{result['passed']}/{run}"
    if not result["failed"]:
        return text
    listed = f"conformance/expected/{result['suite']}.txt"
    return f"{text} ({result['failed']} known to fail, [listed]({listed}))"


def table() -> str:
    rows = [
        "| Specification | Suite | Passing |",
        "| --- | --- | --- |",
    ]
    for suite, about in SUITES.items():
        path = ROOT / "results" / f"{suite}.json"
        if not path.exists():
            sys.exit(f"no results for {suite}: run conformance/run.sh")
        result = json.loads(path.read_text())
        rows.append(f"| {about['spec']} | {about['suite']} | {passing(result)} |")
    return "\n".join(rows)


def readme(check: bool) -> int:
    text = README.read_text()
    start, end = text.index(START) + len(START), text.index(END)
    updated = f"{text[:start]}\n{table()}\n{text[end:]}"
    if updated == text:
        return 0
    if check:
        print("README's conformance table is out of date: run `make conformance`")
        return 1
    README.write_text(updated)
    return 0


if __name__ == "__main__":
    if sys.argv[1:2] == ["readme"]:
        sys.exit(readme("--check" in sys.argv))
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    sys.exit(record(sys.argv[1], Path(sys.argv[2]), sys.argv[3]))
