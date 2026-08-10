#!/usr/bin/env python3
"""Mutation / lifecycle differential-parity runner.

The bash harness (`differential-parity.sh`) covers read fixtures and the
unauthenticated auth surface. This runner covers the *mutating* surface — the
POST/PUT/PATCH/DELETE operations — which need stateful chains: create a resource,
capture the per-server id it returns, exercise it, then delete it. Each chain runs
independently against the legacy oracle and the Rust server; a resource created on
one server never has to match the id on the other, because we compare response
*shape*, not values (same rule as the read harness).

Chains live in contracts/parity-lifecycles.json. Every chain should end by deleting
what it created, so a run leaves both databases as it found them (writes are
transient — this is "drive the app", not "seed data").

Shape logic mirrors shapes_verdict() in the bash harness: a skeleton reduces each
value to its type name (objects keep sorted keys, arrays collapse to their first
element), and two skeletons are "compatible" when they differ only by a null or an
empty container on one side (an unavoidable two-dataset artifact). The accepted-
divergence allowlist (contracts/parity-accepted-divergences.json) is honoured, so
a documented policy divergence does not fail the gate.

Exit 0 iff no unexpected divergence.
"""
import json
import os
import sys
import urllib.request
import urllib.parse
import urllib.error
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LIFECYCLES = ROOT / "contracts" / "parity-lifecycles.json"
ACCEPTED = ROOT / "contracts" / "parity-accepted-divergences.json"
REPORT = ROOT / "contracts" / "parity-mutations-report.json"

LEGACY_BASE = os.environ.get("LEGACY_BASE", "http://127.0.0.1:8000")
RUST_BASE = os.environ.get("RUST_BASE", "http://127.0.0.1:3100")
TIMEOUT = int(os.environ.get("REQ_TIMEOUT", "15"))


def creds(prefix):
    user = os.environ.get(f"{prefix}_USER") or os.environ.get("PARITY_USER")
    pw = os.environ.get(f"{prefix}_PASS") or os.environ.get("PARITY_PASS")
    return user, pw


def login(base, user, pw):
    data = urllib.parse.urlencode({"username": user, "password": pw}).encode()
    req = urllib.request.Request(
        f"{base}/api/v1/auth/token", data=data,
        headers={"content-type": "application/x-www-form-urlencoded"}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT) as r:
            return json.load(r).get("access_token")
    except urllib.error.HTTPError as e:
        raise SystemExit(f"login failed at {base}: {e.code} {e.read()[:200]!r}")


def _build_multipart(fields):
    """Encode a multipart/form-data body. A field value that is a dict with a
    `content` key becomes a file part; anything else is a plain text field."""
    boundary = "----ParityBoundary7MA4YWxkTrZu0gW"
    out = []
    for name, val in fields.items():
        if isinstance(val, dict) and "content" in val:
            fn = val.get("filename", "file")
            ct = val.get("content_type", "application/octet-stream")
            out.append(
                f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"; '
                f'filename="{fn}"\r\nContent-Type: {ct}\r\n\r\n{val["content"]}\r\n')
        else:
            out.append(
                f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"'
                f'\r\n\r\n{val}\r\n')
    out.append(f"--{boundary}--\r\n")
    return "".join(out).encode(), f"multipart/form-data; boundary={boundary}"


def request(base, method, path, token, body, form=False, multipart=None, sse=False):
    url = f"{base}{path}"
    data = None
    headers = {}
    if token:
        headers["authorization"] = f"Bearer {token}"
    if sse:
        # A stream has no response body to skeletonise; assert it opens with the
        # right status and content-type, then close without draining the stream.
        req = urllib.request.Request(url, headers=headers, method=method)
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return str(r.status), {"content_type": r.headers.get("content-type", "")}
        except urllib.error.HTTPError as e:
            return str(e.code), None
        except Exception:  # noqa: BLE001
            return "000", None
    if multipart is not None:
        data, headers["content-type"] = _build_multipart(multipart)
    elif body is not None:
        if form:
            data = urllib.parse.urlencode(body).encode()
            headers["content-type"] = "application/x-www-form-urlencoded"
        else:
            data = json.dumps(body).encode()
            headers["content-type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT) as r:
            raw = r.read()
            status = r.status
    except urllib.error.HTTPError as e:
        raw = e.read()
        status = e.code
    except Exception as e:  # noqa: BLE001 — network/timeout -> record as 000
        return "000", None
    try:
        parsed = json.loads(raw) if raw else None
    except json.JSONDecodeError:
        parsed = f"<non-json:{len(raw)}>"
    return str(status), parsed


def skeleton(v):
    if isinstance(v, dict):
        return {k: skeleton(v[k]) for k in sorted(v)}
    if isinstance(v, list):
        return [] if not v else [skeleton(v[0])]
    if v is None:
        return "null"
    if isinstance(v, bool):
        return "boolean"
    if isinstance(v, (int, float)):
        return "number"
    if isinstance(v, str):
        return "string"
    return "unknown"


def compat(a, b):
    """Same rule as bash shapes_verdict: null/empty-container is a wildcard."""
    if a == "null" or b == "null":
        return True
    if isinstance(a, list) and isinstance(b, list):
        if not a or not b:
            return True
        return compat(a[0], b[0])
    if isinstance(a, dict) and isinstance(b, dict):
        if not a or not b:
            return True
        return sorted(a) == sorted(b) and all(compat(a[k], b[k]) for k in a)
    return a == b


def verdict(a, b):
    if a == b:
        return "equal"
    return "compatible" if compat(a, b) else "differ"


def substitute(obj, vars_):
    """Replace {{var}} tokens in strings, recursively, from vars_ dict."""
    if isinstance(obj, str):
        for k, val in vars_.items():
            obj = obj.replace("{{" + k + "}}", str(val))
        return obj
    if isinstance(obj, dict):
        return {k: substitute(v, vars_) for k, v in obj.items()}
    if isinstance(obj, list):
        return [substitute(v, vars_) for v in obj]
    return obj


def dig(value, path):
    """Extract a jq-ish dotted path like '.id' or '.items[0].id' from parsed JSON."""
    cur = value
    for part in path.strip(".").split("."):
        if not part:
            continue
        idx = None
        if "[" in part:
            part, rest = part.split("[", 1)
            idx = int(rest.rstrip("]"))
        if isinstance(cur, dict):
            cur = cur.get(part)
        if idx is not None and isinstance(cur, list):
            cur = cur[idx] if idx < len(cur) else None
    return cur


def accepted_rules():
    if not ACCEPTED.exists():
        return []
    return json.loads(ACCEPTED.read_text()).get("rules", [])


def is_accepted(rec, rules):
    for r in rules:
        if (r.get("legacy_status") == rec["legacy_status"]
                and r.get("rust_status") == rec["rust_status"]
                and r.get("method", rec["method"]) == rec["method"]
                and r.get("path", rec["path"]) == rec["path"]):
            return True
    return False


_NONCE = [0]


def _fresh_nonce():
    # Unique per chain invocation so throwaway resources (e.g. test users, whose
    # email/handle are unique) never collide across runs. os.getpid keeps it
    # distinct between concurrent runs; the counter distinguishes chains.
    _NONCE[0] += 1
    return f"{os.getpid()}{_NONCE[0]:03d}"


def run_chain(chain, ltok, rtok):
    nonce = _fresh_nonce()
    lvars, rvars = {"nonce": nonce}, {"nonce": nonce}
    results = []
    for step in chain["steps"]:
        name = step["name"]
        method = step["method"]
        auth = step.get("auth", False)
        form = step.get("form", False)
        # A step may authenticate with a token captured earlier (e.g. a throwaway
        # user's own session) via `token_var`, instead of the shared admin token.
        tvar = step.get("token_var")
        if tvar:
            lt = lvars.get(tvar)
            rt = rvars.get(tvar)
        else:
            lt = ltok if auth else None
            rt = rtok if auth else None
        lpath = substitute(step["path"], lvars)
        rpath = substitute(step["path"], rvars)
        lbody = substitute(step["body"], lvars) if "body" in step else None
        rbody = substitute(step["body"], rvars) if "body" in step else None
        lmp = substitute(step["multipart"], lvars) if "multipart" in step else None
        rmp = substitute(step["multipart"], rvars) if "multipart" in step else None
        sse = step.get("sse", False)
        ls, ljson = request(LEGACY_BASE, method, lpath, lt, lbody, form, lmp, sse)
        rs, rjson = request(RUST_BASE, method, rpath, rt, rbody, form, rmp, sse)
        for var, jp in step.get("capture", {}).items():
            lval = dig(ljson, jp)
            rval = dig(rjson, jp)
            if lval is not None:
                lvars[var] = lval
            if rval is not None:
                rvars[var] = rval
        detail = ""
        if ls != rs:
            match = False
            detail = f"status {ls}≠{rs}"
        else:
            v = verdict(skeleton(ljson), skeleton(rjson))
            if v == "equal":
                match = True
            elif v == "compatible":
                match = True
                detail = "shape-compatible (null/empty on one side)"
            else:
                match = False
                detail = "body-shape differs"
        results.append({
            "operation": f"{chain['chain']}:{name}",
            "method": method,
            "path": step["path"],
            "legacy_status": ls,
            "rust_status": rs,
            "parity": match,
            "detail": detail,
        })
    return results


def main():
    luser, lpw = creds("LEGACY")
    ruser, rpw = creds("RUST")
    if not (luser and lpw and ruser and rpw):
        raise SystemExit("error: LEGACY_USER/PASS and RUST_USER/PASS (or PARITY_USER/PASS) required")
    print(f"parity-mutations  legacy={LEGACY_BASE}  rust={RUST_BASE}")
    ltok = login(LEGACY_BASE, luser, lpw)
    rtok = login(RUST_BASE, ruser, rpw)
    if not ltok or not rtok:
        raise SystemExit("error: could not sign in to both servers")

    chains = json.loads(LIFECYCLES.read_text())
    rules = accepted_rules()
    results = []
    for chain in chains:
        results.extend(run_chain(chain, ltok, rtok))

    for rec in results:
        rec["accepted"] = (not rec["parity"]) and is_accepted(rec, rules)

    total = len(results)
    ok = sum(1 for r in results if r["parity"])
    diverged = [r for r in results if not r["parity"]]
    accepted = [r for r in diverged if r["accepted"]]
    unexpected = [r for r in diverged if not r["accepted"]]

    REPORT.write_text(json.dumps({
        "total": total, "parity": ok,
        "diverged": len(diverged), "accepted": len(accepted),
        "unexpected": len(unexpected),
        "operations": sorted(results, key=lambda r: (r["path"], r["method"])),
    }, indent=2))

    print(f"\nmutation parity {ok}/{total}  "
          f"(diverged: {len(diverged)} — accepted: {len(accepted)}, unexpected: {len(unexpected)})"
          f"  →  {REPORT.relative_to(ROOT)}")
    if diverged:
        print()
        for r in sorted(diverged, key=lambda r: (not r["accepted"], r["path"])):
            tag = "ok  " if r["accepted"] else "FAIL"
            print(f"  {tag} {r['method']:6} {r['path'][:50]:50} {r['legacy_status']}|{r['rust_status']}  {r['detail']}")
    if unexpected:
        print(f"\nFAIL: {len(unexpected)} unexpected mutation divergence(s)")
        sys.exit(1)
    print(f"\nAll {len(diverged)} divergence(s) accepted by policy; no unexpected divergence."
          if diverged else "All mutation chains agree.")


if __name__ == "__main__":
    main()
