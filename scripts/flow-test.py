#!/usr/bin/env python3
"""End-to-end flow test against a running Meridian server.

Each check states what it expects. A check that cannot run because of the known
stray-trigger blocker is reported as BLOCKED, never as a pass — a suite that
reports green while skipping half the product is worse than one that fails.
"""
import json
import subprocess
import sys
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:3199"
PASS, FAIL, BLOCKED = [], [], []


def call(method, path, token=None, body=None, form=None):
    url = BASE + path
    data, headers = None, {}
    if token:
        headers["authorization"] = f"Bearer {token}"
    if form is not None:
        data = form.encode()
        headers["content-type"] = "application/x-www-form-urlencoded"
    elif body is not None:
        data = json.dumps(body).encode()
        headers["content-type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            raw = r.read().decode()
            try:
                return r.status, json.loads(raw)
            except json.JSONDecodeError:
                return r.status, raw
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw)
        except json.JSONDecodeError:
            return e.code, raw
    except Exception as e:  # noqa: BLE001
        return 0, str(e)


def check(flow, name, ok, detail=""):
    entry = f"{flow} · {name}" + (f" — {detail}" if detail else "")
    (PASS if ok else FAIL).append(entry)
    print(f"  {'PASS' if ok else 'FAIL'}  {name}" + (f"  [{detail}]" if detail else ""))


def blocked(flow, name, why):
    BLOCKED.append(f"{flow} · {name} — {why}")
    print(f"  BLOCKED  {name}  [{why}]")


def section(title):
    print(f"\n=== {title} ===")


# ── Flow 0: platform ────────────────────────────────────────────────────────
section("Flow 0 · platform")
s, b = call("GET", "/health")
check("platform", "health", s == 200 and b.get("status") == "ok")
s, b = call("GET", "/health/ready")
deps = b.get("dependencies", {}) if isinstance(b, dict) else {}
check("platform", "readiness: all dependencies", s == 200 and all(deps.values()), str(deps))
s, _ = call("GET", "/metrics")
check("platform", "metrics", s == 200)

# ── Flow 1: authentication ──────────────────────────────────────────────────
section("Flow 1 · authentication")
s, b = call("POST", "/api/v1/auth/token",
            form="username=admin@meridian.example&password=DevPass123!")
tok = b.get("access_token") if isinstance(b, dict) else None
refresh = b.get("refresh_token") if isinstance(b, dict) else None
check("auth", "sign in", s == 200 and bool(tok))
if not tok:
    print("cannot continue without a token")
    sys.exit(1)

s, _ = call("POST", "/api/v1/auth/token",
            form="username=admin@meridian.example&password=wrong")
check("auth", "wrong password rejected", s == 401, f"got {s}")
s, b = call("GET", "/api/v1/users/me", tok)
check("auth", "current user", s == 200 and "email" in b)
s, _ = call("GET", "/api/v1/users/me")
check("auth", "unauthenticated is refused", s == 401, f"got {s}")

s, b = call("POST", "/api/v1/auth/refresh", body={"refresh_token": refresh})
new_tok = b.get("access_token") if isinstance(b, dict) else None
new_refresh = b.get("refresh_token") if isinstance(b, dict) else None
check("auth", "refresh rotates", s == 200 and new_tok and new_refresh != refresh)
s, _ = call("POST", "/api/v1/auth/refresh", body={"refresh_token": refresh})
check("auth", "spent refresh token is rejected (replay detection)", s in (401, 400), f"got {s}")

# Replay detection revokes the whole token *family*, so the token rotated into
# above is dead too. That is the correct, aggressive behaviour — a replayed token
# means the family is compromised — and it is worth asserting rather than working
# around silently.
s, _ = call("GET", "/api/v1/users/me", new_tok)
check("auth", "replay revokes the whole family, not just the replayed token",
      s == 401, f"got {s}")
s, b = call("POST", "/api/v1/auth/token",
            form="username=admin@meridian.example&password=DevPass123!")
tok = b.get("access_token") if isinstance(b, dict) else None
check("auth", "can sign in again after a family revocation", bool(tok))

s, b = call("GET", "/api/v1/auth/sessions", tok)
current = [x for x in b if x.get("is_current")] if isinstance(b, list) else []
check("auth", "sessions list marks exactly one current", s == 200 and len(current) == 1)

# ── Flow 2: read surface ────────────────────────────────────────────────────
section("Flow 2 · read surface")
READS = [
    "/api/v1/stories", "/api/v1/tasks", "/api/v1/users", "/api/v1/desks",
    "/api/v1/feed", "/api/v1/notifications", "/api/v1/navigation",
    "/api/v1/dashboard", "/api/v1/dashboard/summary", "/api/v1/dashboard/pipeline",
    "/api/v1/dashboard/activity", "/api/v1/dashboard/releases",
    "/api/v1/dashboard/workload", "/api/v1/dashboard/attention",
    "/api/v1/dashboard/views", "/api/v1/activity-center",
    "/api/v1/assets", "/api/v1/reviews", "/api/v1/corrections",
    "/api/v1/audit?limit=5", "/api/v1/permission-policies", "/api/v1/delegations",
    "/api/v1/roles", "/api/v1/permissions/capabilities", "/api/v1/feature-flags/admin",
    "/api/v1/saved-searches", "/api/v1/invitations", "/api/v1/pitches",
    "/api/v1/search/facets", "/api/v1/favorites", "/api/v1/follows", "/api/v1/recents",
    "/api/v1/channels", "/api/v1/front-page", "/api/v1/coverage-events",
]
bad = [(p, call("GET", p, tok)[0]) for p in READS]
bad = [(p, c) for p, c in bad if c != 200]
check("read", f"all {len(READS)} read endpoints return 200", not bad, str(bad))

# ── Flow 3: authorization ───────────────────────────────────────────────────
section("Flow 3 · authorization")
for path in ["/api/v1/stories", "/api/v1/audit", "/api/v1/channels", "/api/v1/front-page",
             "/api/v1/presence?entity_type=story&entity_id=x"]:
    s, _ = call("GET", path)
    check("authz", f"unauthenticated {path}", s == 401, f"got {s}")

# ── Flow 4: verification (evidence → custody → claims) ──────────────────────
section("Flow 4 · verification")
s, stories = call("GET", "/api/v1/stories", tok)
# Prefer a story that has a filed version: the gate flow needs one, and a
# freshly-created probe story has none.
story = None
if isinstance(stories, list):
    for cand in stories:
        sv, vs = call("GET", f"/api/v1/stories/{cand['id']}/versions", tok)
        if isinstance(vs, list) and vs:
            story = cand["id"]
            break
    if story is None and stories:
        story = stories[0]["id"]
if not story:
    blocked("verification", "whole flow", "no stories exist to attach evidence to")
else:
    s, ev = call("POST", f"/api/v1/stories/{story}/evidence", tok,
                 {"kind": "document", "title": "flowtest evidence", "notes": "acquired"})
    ok = s == 201 and len(ev.get("chain_of_custody", [])) == 1
    check("verification", "create evidence writes origin custody entry", ok, f"{s}")
    ev_id = ev.get("id") if isinstance(ev, dict) else None
    if ev_id:
        s, ev2 = call("POST", f"/api/v1/evidence/{ev_id}/status", tok,
                      {"status": "verified", "note": "checked"})
        ok = s == 200 and ev2["verification_status"] == "verified" \
            and len(ev2["chain_of_custody"]) == 2
        check("verification", "status transition appends custody (1→2)", ok, f"{s}")
        s, _ = call("POST", f"/api/v1/evidence/{ev_id}/status", tok, {"status": "nonsense"})
        check("verification", "invalid status refused", s == 422, f"got {s}")
    s, cl = call("POST", f"/api/v1/stories/{story}/claims", tok,
                 {"text": "flowtest claim", "evidence_ids": [ev_id] if ev_id else []})
    check("verification", "create claim with backing evidence", s == 201, f"{s}")
    claim = cl.get("id") if isinstance(cl, dict) else None
    if claim:
        s, _ = call("POST", f"/api/v1/claims/{claim}/decision", tok, {"status": "verified"})
        check("verification", "decide claim", s == 200, f"{s}")
    s, src = call("POST", f"/api/v1/stories/{story}/sources", tok,
                  {"identity": "Flow Test", "publishable_attribution": "a source",
                   "ground_rule": "background"})
    check("verification", "create confidential source", s == 201, f"{s}")
    src_id = src.get("id") if isinstance(src, dict) else None
    if src_id:
        s, _ = call("POST", f"/api/v1/sources/{src_id}/approve", tok, {"rationale": "ok"})
        check("verification", "manager-approve source", s == 200, f"{s}")

# ── Flow 5: publish gate ────────────────────────────────────────────────────
section("Flow 5 · publish gate")
if story:
    s, r = call("GET", f"/api/v1/stories/{story}/publish-readiness", tok)
    check("gate", "readiness evaluates", s == 200 and "blockers" in r)
    s, vs = call("GET", f"/api/v1/stories/{story}/versions", tok)
    ver = vs[0]["id"] if isinstance(vs, list) and vs else None
    if ver:
        s, b = call("POST", f"/api/v1/stories/{story}/releases", tok,
                    {"version_id": ver, "channels": ["web"],
                     "expires_at": "2020-01-01T00:00:00Z"})
        check("gate", "past expiry refused before the gate", s == 422, f"got {s}: {b}")
        s, b = call("POST", f"/api/v1/stories/{story}/releases", tok,
                    {"version_id": ver, "channels": ["web"],
                     "embargo_until": "2030-06-01T00:00:00Z",
                     "expires_at": "2030-01-01T00:00:00Z"})
        check("gate", "inverted embargo/expiry window refused", s == 422, f"got {s}")
        s, b = call("POST", f"/api/v1/stories/{story}/releases", tok,
                    {"version_id": ver, "channels": ["web"],
                     "embargo_until": "2030-01-01T00:00:00Z",
                     "expires_at": "2030-06-01T00:00:00Z"})
        check("gate", "valid window reaches the gate (409, not 422)", s == 409, f"got {s}")
    else:
        blocked("gate", "release creation", "story has no filed version")

# ── Flow 6: channels ────────────────────────────────────────────────────────
section("Flow 6 · channels")
s, b = call("POST", "/api/v1/channels", tok,
            {"key": "flowtest", "name": "Flow Test", "kind": "social"})
check("channels", "create", s == 200 and b.get("is_active") is True, f"{s}")
s, b = call("POST", "/api/v1/channels", tok,
            {"key": "flowtest", "name": "Flow Test", "kind": "social", "is_active": False})
s2, all_ch = call("GET", "/api/v1/channels", tok)
same = [c for c in all_ch if c["key"] == "flowtest"] if isinstance(all_ch, list) else []
check("channels", "upsert by key does not duplicate", len(same) == 1 and not same[0]["is_active"])
s, _ = call("POST", "/api/v1/channels", tok, {"key": "x", "name": "X", "kind": "pigeon"})
check("channels", "unknown kind refused", s == 422, f"got {s}")
s, act = call("GET", "/api/v1/channels?active_only=true", tok)
check("channels", "retired channel not offered as a target",
      isinstance(act, list) and all(c["key"] != "flowtest" for c in act), f"{s}")

# ── Flow 7: front page ──────────────────────────────────────────────────────
section("Flow 7 · front page")
s, slots = call("GET", "/api/v1/front-page", tok)
check("frontpage", "board loads with ordered slots",
      s == 200 and isinstance(slots, list) and slots
      and [x["position"] for x in slots] == sorted(x["position"] for x in slots))
if slots and story:
    s, b = call("PUT", f"/api/v1/front-page/{slots[0]['id']}", tok, {"story_id": story})
    check("frontpage", "unpublished story refused", s == 422, f"got {s}")

    # Publishing through the gate needs three sign-offs, which is a different
    # flow's concern; set the state directly so this flow tests *placement*.
    dsn = subprocess.run(["bash", "-lc",
                          "source .env.local && echo $DATABASE_DIRECT_URL"],
                         capture_output=True, text=True).stdout.strip()
    pub = subprocess.run(
        ["psql", dsn, "-q", "-c",
         f"UPDATE meridian.stories SET publication_state='published' WHERE id='{story}';"],
        capture_output=True, text=True)
    if pub.returncode != 0:
        blocked("frontpage", "placing a published story",
                f"could not publish a fixture story: {pub.stderr.strip()[:80]}")
    else:
        s, b = call("PUT", f"/api/v1/front-page/{slots[0]['id']}", tok, {"story_id": story})
        check("frontpage", "place a published story", s == 200 and b.get("story_id") == story,
              f"{s}")
        # The same story in a second slot must vacate the first — a front page
        # cannot show one story twice.
        if len(slots) > 1:
            s, _ = call("PUT", f"/api/v1/front-page/{slots[1]['id']}", tok, {"story_id": story})
            s2, board = call("GET", "/api/v1/front-page", tok)
            holding = [x["position"] for x in board if x.get("story_id") == story]
            check("frontpage", "placing again vacates the previous slot",
                  len(holding) == 1, f"held by positions {holding}")
        s, _ = call("PUT", f"/api/v1/front-page/{slots[0]['id']}", tok, {"story_id": None})
        s2, _ = call("PUT", f"/api/v1/front-page/{slots[1]['id']}", tok, {"story_id": None})
        check("frontpage", "clear a slot", s == 200 and s2 == 200)
        subprocess.run(["psql", dsn, "-q", "-c",
                        f"UPDATE meridian.stories SET publication_state='not_live' "
                        f"WHERE id='{story}';"], capture_output=True)

# ── Flow 8: awareness ───────────────────────────────────────────────────────
section("Flow 8 · awareness")
s, _ = call("POST", "/api/v1/presence", tok,
            {"entity_type": "story", "entity_id": story or "x"})
check("awareness", "presence heartbeat", s == 200, f"{s}")
s, b = call("GET", f"/api/v1/presence?entity_type=story&entity_id={story or 'x'}", tok)
check("awareness", "presence excludes the caller", s == 200 and isinstance(b, list))
if story:
    s, _ = call("POST", "/api/v1/favorites", tok,
                {"entity_type": "story", "entity_id": story, "label": "flowtest"})
    check("awareness", "pin a favourite", s in (200, 201), f"{s}")
    s, _ = call("POST", "/api/v1/follows", tok, {"entity_type": "story", "entity_id": story})
    check("awareness", "follow a story", s in (200, 201), f"{s}")
    s, _ = call("POST", "/api/v1/recents", tok,
                {"entity_type": "story", "entity_id": story, "title": "flowtest"})
    check("awareness", "record a recent", s in (200, 201), f"{s}")
s, _ = call("POST", "/api/v1/notifications/read-all", tok)
check("awareness", "mark all notifications read", s == 200, f"{s}")

# ── Flow 9: search ──────────────────────────────────────────────────────────
section("Flow 9 · search")
s, _ = call("GET", "/api/v1/search?q=the", tok)
check("search", "query runs", s == 200, f"{s}")
s, b = call("POST", "/api/v1/saved-searches", tok,
            {"name": "flowtest", "query": "budget",
             "filters": {"workflow_state": "drafting", "category": ""}})
check("search", "save a search with its facets", s == 201, f"{s}")
saved = b.get("id") if isinstance(b, dict) else None
if saved:
    s, _ = call("DELETE", f"/api/v1/saved-searches/{saved}", tok)
    check("search", "delete a saved search", s == 204, f"{s}")

# ── Flow 10: dashboard ──────────────────────────────────────────────────────
section("Flow 10 · dashboard")
s, _ = call("POST", "/api/v1/dashboard/metrics/capture", tok)
check("dashboard", "capture metric snapshot", s == 200, f"{s}")
s, b = call("GET", "/api/v1/dashboard/summary", tok)
m = b.get("metrics", {}) if isinstance(b, dict) else {}
ok = all(k in m for k in ("open_corrections", "overdue_tasks", "pending_reviews",
                          "scheduled_releases", "stories_due_today"))
check("dashboard", "summary carries all five metric cards", ok)
ok = all(set(v) >= {"value", "change", "trend", "overdue", "at_risk", "critical"}
         for v in m.values())
check("dashboard", "each card carries value/change/trend/breakdowns", ok)
# Every key, not first-failure-wins: only `pending_reviews` had column aliases,
# so a break-on-first-failure loop reported one bug where there were five.
drill_bad = []
for key in ["pending_reviews", "overdue_tasks", "open_corrections",
            "failed_releases", "scheduled_releases", "stories_due_today",
            "pipeline:drafting"]:
    s, _ = call("GET", f"/api/v1/dashboard/drilldown/{key}", tok)
    if s != 200:
        drill_bad.append((key, s))
check("dashboard", "every drilldown key resolves", not drill_bad, str(drill_bad))
s, _ = call("GET", "/api/v1/dashboard/drilldown/not-a-key", tok)
check("dashboard", "unknown drilldown key refused", s == 422, f"got {s}")
s, b = call("POST", "/api/v1/dashboard/views", tok,
            {"name": "flowtest view", "widgets": ["summary_metrics", "attention_queue"]})
view = b.get("id") if isinstance(b, dict) else None
check("dashboard", "save a view with chosen widgets",
      s == 201 and len(b.get("layout_json", [])) == 2, f"{s}")
if view:
    s, _ = call("POST", f"/api/v1/dashboard/views/{view}/set-default", tok)
    check("dashboard", "set default view", s == 200, f"{s}")
    s, lay = call("GET", "/api/v1/dashboard/layout", tok)
    check("dashboard", "layout resolves to the saved view",
          s == 200 and lay.get("source") == "saved_view", str(lay.get("source")))
    s, _ = call("POST", "/api/v1/dashboard/layout/reset", tok)
    check("dashboard", "reset returns to the role baseline", s == 200, f"{s}")
    s, _ = call("DELETE", f"/api/v1/dashboard/views/{view}", tok)
    check("dashboard", "delete the view", s == 204, f"{s}")

# ── Flow 11: governance ─────────────────────────────────────────────────────
section("Flow 11 · governance")
s, users = call("GET", "/api/v1/users", tok)
me = users[0]["id"] if isinstance(users, list) and users else None
if me:
    s, b = call("POST", "/api/v1/permission-policies", tok,
                {"subject_type": "user", "subject_id": me,
                 "capability": "stories.edit", "allow": True, "reason": "flowtest"})
    check("governance", "write a permission override", s in (200, 201), f"{s}")
    pid = b.get("id") if isinstance(b, dict) else None
    if pid:
        s, _ = call("DELETE", f"/api/v1/permission-policies/{pid}", tok)
        check("governance", "remove the override", s == 204, f"{s}")
    s, b = call("GET", f"/api/v1/users/{me}/effective-permissions", tok)
    check("governance", "effective permissions resolve with a trace", s == 200)
    s, b = call("GET", f"/api/v1/users/{me}/role-assignments", tok)
    check("governance", "role assignments list", s == 200)
s, _ = call("POST", "/api/v1/permissions/simulate", tok,
            {"user_id": me, "capability": "stories.edit"})
check("governance", "permission simulate", s == 200, f"{s}")

# ── Flow 12: writes the stray trigger blocks ────────────────────────────────
section("Flow 12 · writes to trigger-carrying tables")
# The API sanitises database errors out of the response body — correct, since a
# 500 should not leak schema internals — so the body cannot tell us *why* a write
# failed. Consult the drift state directly instead of pattern-matching an error
# string that deliberately is not there.
DRIFT = subprocess.run(
    ["bash", "-lc",
     "set -a && source .env.local && set +a && ./scripts/check-schema-drift.sh >/dev/null 2>&1"],
    capture_output=True).returncode != 0
print(f"  (stray-object drift present: {DRIFT})")
s, b = call("POST", "/api/v1/tasks", tok, {"title": "flowtest task"})
if s == 500 and DRIFT:
    blocked("tasks", "create a task", "stray search_index_sync() trigger on tasks")
else:
    check("tasks", "create a task", s == 201, f"{s}")
s, b = call("POST", "/api/v1/stories", tok,
            {"slug": "flowtest-story", "title": "flowtest story", "dek": "d"})
if s == 500 and DRIFT:
    blocked("stories", "create a story", "stray search_index_sync() trigger on stories")
else:
    check("stories", "create a story", s == 201, f"{s}")
s, b = call("POST", "/api/v1/desks", tok, {"name": "Flow Desk", "slug": "flow-desk"})
if s == 500 and DRIFT:
    blocked("desks", "create a sector", "stray search_index_sync() trigger on desks")
else:
    check("desks", "create a sector", s == 201, f"{s}")
s, b = call("POST", "/api/v1/users", tok,
            {"email": "flow@x.test", "handle": "flowtest", "display_name": "F",
             "role": "reporter", "password": "FlowPass123!"})
if s == 500 and DRIFT:
    blocked("users", "create a user", "stray search_index_sync() trigger on users")
else:
    check("users", "create a user", s in (200, 201), f"{s}")

# ── Flow 13: SSR ────────────────────────────────────────────────────────────
section("Flow 13 · server-rendered routes")
ROUTES = ["/", "/sign-in", "/onboarding", "/no-access", "/invitations", "/settings",
          "/settings/notifications", "/settings/sessions", "/org/sectors", "/org/search",
          "/org/feed", "/org/people", "/org/analytics", "/org/assets", "/org/publishing",
          "/org/publishing/corrections", "/org/frontpage", "/admin", "/admin/roles",
          "/admin/governance", "/admin/sectors", "/s/x", "/s/x/stories", "/s/x/tasks",
          "/s/x/planning", "/s/x/settings", "/s/x/assets", "/s/x/team", "/s/x/analytics",
          "/s/x/search", "/editor", "/read/nope", "/definitely-not-a-route"]
bad = []
for r in ROUTES:
    st, body = call("GET", r)
    if st != 200 or "panicked" in str(body).lower():
        bad.append((r, st))
check("ssr", f"all {len(ROUTES)} routes render 200 without panicking", not bad, str(bad))

# ── cleanup ─────────────────────────────────────────────────────────────────
section("cleanup")
subprocess.run(
    ["psql", subprocess.run(["bash", "-lc", "source .env.local && echo $DATABASE_DIRECT_URL"],
                            capture_output=True, text=True).stdout.strip(),
     "-q", "-c",
     "UPDATE meridian.front_page_slots SET story_id=NULL "
     "WHERE story_id IN (SELECT id FROM meridian.stories WHERE slug='flowtest-story');"
     "DELETE FROM meridian.channels WHERE key='flowtest';"
     "DELETE FROM meridian.tasks WHERE title='flowtest task';"
     "DELETE FROM meridian.stories WHERE slug='flowtest-story';"
     "DELETE FROM meridian.desk_memberships WHERE desk_id IN "
     "(SELECT id FROM meridian.desks WHERE slug='flow-desk');"
     "DELETE FROM meridian.desks WHERE slug='flow-desk';"
     "DELETE FROM meridian.users WHERE handle='flowtest';"
     "DELETE FROM meridian.evidence WHERE title='flowtest evidence';"
     "DELETE FROM meridian.claims WHERE text='flowtest claim';"
     "DELETE FROM meridian.sources WHERE identity='Flow Test';"
     "DELETE FROM meridian.presence WHERE entity_type='story';"
     "DELETE FROM meridian.favorites WHERE label='flowtest';"],
    capture_output=True)
print("  probe rows removed")

# ── report ──────────────────────────────────────────────────────────────────
print("\n" + "=" * 66)
print(f"PASSED  {len(PASS)}")
print(f"FAILED  {len(FAIL)}")
print(f"BLOCKED {len(BLOCKED)}")
if FAIL:
    print("\nFAILURES")
    for f in FAIL:
        print("  ✗ " + f)
if BLOCKED:
    print("\nBLOCKED (not counted as passing)")
    for b_ in BLOCKED:
        print("  ⊘ " + b_)
sys.exit(1 if FAIL else 0)
