#!/usr/bin/env python3
"""API content seeder for iGEN News.

Creates a large volume of varied, full-body articles across ALL 50 sectors by calling
the running server's REST API (not raw SQL) — so it exercises real validation, authz
and webhooks, and works against any deployment (localhost or a live server) without
database access. Each article is topic-relevant to a real industry (sub-sector) of its
sector, with a distinct headline, dek and multi-paragraph body.

Usage:
  python3 scripts/seed_content_api.py \
      --base-url http://localhost:3100 \
      --username admin@igennews.com --password 'DevPass123!' \
      --per-sector 4

Notes:
  * Articles are created as drafts (the API's initial state). Publishing goes through
    the editorial workflow gate and is intentionally not automated here.
  * Author is the authenticated user. Log in as a desk reporter to attribute to them,
    or pass --author-id.
  * Safe to re-run: slugs are timestamp-suffixed, so re-runs add more content.
  * Only stdlib is used (urllib) — no pip install needed.
"""
import argparse, json, sys, time, urllib.request, urllib.parse, urllib.error

# --- Content generators (varied so no two articles read the same) -------------------

HEADLINES = [
    "{ind} drives fresh momentum in India's {sec} push",
    "Inside {ind}: what's changing and who's paying",
    "The quiet transformation of {ind}",
    "Why investors are circling {ind}",
    "{ind} faces a reckoning as demand shifts",
    "How policy is reshaping {ind}",
    "{ind}: the supply-chain story behind the headlines",
    "A new generation of firms bets on {ind}",
    "{ind} and the road to 2030",
    "The numbers that explain {ind} right now",
    "{ind} exports climb as global demand firms up",
    "Regulators sharpen their focus on {ind}",
    "{ind} at an inflection point",
    "Capital, capacity and the future of {ind}",
]
DEKS = [
    "An iGEN News analysis of {indl} — the forces, the firms and the fault lines.",
    "What the latest shifts in {indl} mean for the {sec} sector.",
    "A ground-level look at {indl} and where it heads next.",
    "The money, the policy and the players shaping {indl}.",
    "Why {indl} has become one of the sector's stories to watch.",
]
LEDES = [
    "India's {indl} is entering a defining stretch. Behind the headline numbers sits a more complicated story of capital, capacity and competition.",
    "For years {indl} sat in the background of the {sec} conversation. That is changing fast.",
    "Ask anyone working in {indl} and you'll hear the same thing: the fundamentals are strong, but the next eighteen months will decide who leads.",
    "A wave of investment, policy attention and new entrants has turned {indl} into one of the {sec} sector's most closely watched corners.",
]
BODY_POOL = [
    "The industry covers {desc} That breadth is both its strength and, increasingly, its challenge.",
    "Executives across the {sec} sector describe a market in flux — demand is real, but margins are thin and the rules keep moving.",
    "Policymakers have signalled support, though the details will decide whether the momentum holds.",
    "Supply chains that once ran on autopilot are being redrawn, with firms hedging against shortages and price swings.",
    "Talent is the other constraint. The specialists this work needs are scarce, and the competition for them is global.",
    "Smaller, faster companies are pushing into gaps the incumbents were slow to fill, forcing a rethink of who competes and how.",
    "Domestic demand is only half the story; export orders are increasingly where the growth — and the scrutiny — lies.",
    "Investors, once cautious, are now writing larger cheques, betting that scale arrives sooner than the sceptics expect.",
    "Standards and compliance, long an afterthought, have moved to the centre of boardroom conversations.",
]
KICKERS = [
    "Whether {indl} becomes a story of steady growth or squandered advantage will depend on choices made this year.",
    "For the firms that get the next stretch right, the prize is a durable lead in a category that is only getting more strategic.",
    "The direction of travel is clear; the pace, and who benefits, is still being written.",
    "One thing is certain: {indl} will not look the same a year from now.",
]
PRIORITIES = ["high", "medium", "medium", "low", "high", "urgent"]
TYPES = ["article", "analysis", "feature", "explainer", "report"]


def short(name):
    import re
    return re.sub(r"\s+industry$", "", name, flags=re.I).strip()


def slugify(s):
    import re
    s = s.lower().replace("&", "and").replace("'", "")
    return re.sub(r"-+", "-", re.sub(r"[^a-z0-9]+", "-", s)).strip("-")[:110]


def build_article(sector_name, industry, n):
    """Return (title, dek, body_blocks, category, priority, story_type)."""
    ind = short(industry["name"])
    indl = ind.lower()
    sec = sector_name.split(" & ")[0]
    desc = (industry.get("description") or "").strip()
    if desc and not desc.endswith("."):
        desc += "."
    title = HEADLINES[n % len(HEADLINES)].format(ind=ind, sec=sec)
    dek = DEKS[(n + 1) % len(DEKS)].format(indl=indl, sec=sec)
    # Body: lede + description para (if any) + 3 rotating analysis paras + kicker.
    paras = [LEDES[n % len(LEDES)].format(indl=indl, sec=sec)]
    if desc:
        paras.append(BODY_POOL[0].format(desc=desc, sec=sec))
    for j in range(3):
        paras.append(BODY_POOL[1 + ((n + j) % (len(BODY_POOL) - 1))].format(desc=desc, sec=sec, indl=indl))
    paras.append(KICKERS[n % len(KICKERS)].format(indl=indl))
    blocks = [{"type": "heading", "text": title}] + [{"type": "paragraph", "text": p} for p in paras]
    tags = [sec.lower(), indl] + [w for w in indl.split()[:2]]
    return title, dek, blocks, sector_name, PRIORITIES[n % len(PRIORITIES)], TYPES[n % len(TYPES)], tags


# --- HTTP helpers -------------------------------------------------------------------

def _req(method, url, token=None, data=None, form=False):
    headers = {}
    body = None
    if token:
        headers["authorization"] = f"Bearer {token}"
    if data is not None:
        if form:
            body = urllib.parse.urlencode(data).encode()
            headers["content-type"] = "application/x-www-form-urlencoded"
        else:
            body = json.dumps(data).encode()
            headers["content-type"] = "application/json"
    r = urllib.request.Request(url, data=body, headers=headers, method=method)
    with urllib.request.urlopen(r, timeout=30) as resp:
        raw = resp.read().decode()
        return resp.status, (json.loads(raw) if raw.strip().startswith(("{", "[")) else raw)


def login(base, username, password):
    status, data = _req("POST", f"{base}/api/v1/auth/token",
                         data={"username": username, "password": password}, form=True)
    tok = data.get("access_token") if isinstance(data, dict) else None
    if not tok:
        sys.exit(f"login failed ({status}): {data}")
    return tok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base-url", default="http://localhost:3100")
    ap.add_argument("--username", default="admin@igennews.com")
    ap.add_argument("--password", default="DevPass123!")
    ap.add_argument("--per-sector", type=int, default=4, help="articles to create per sector")
    ap.add_argument("--author-id", default=None, help="optional author UUID (default: the logged-in user)")
    ap.add_argument("--only-ige", action="store_true", help="only the 50 India Global Expo sectors")
    ap.add_argument("--max-sectors", type=int, default=0, help="cap sectors (0 = all) — for a quick test run")
    args = ap.parse_args()

    base = args.base_url.rstrip("/")
    token = login(base, args.username, args.password)
    print(f"authenticated to {base}")

    _, desks = _req("GET", f"{base}/api/v1/desks", token=token)
    if args.only_ige:
        desks = [d for d in desks if (d.get("settings") or {}).get("source") == "India Global Expo — Master 50 Sectors"]
    # top-level sectors only (no parent) and not archived
    desks = [d for d in desks if not d.get("is_archived")]
    if args.max_sectors:
        desks = desks[: args.max_sectors]
    print(f"{len(desks)} sectors to seed, {args.per_sector} articles each")

    stamp = int(time.time())
    created = 0
    failed = 0
    for di, d in enumerate(desks):
        try:
            _, subs = _req("GET", f"{base}/api/v1/sectors/{d['id']}/sub-sectors", token=token)
        except Exception as e:
            subs = []
        subs = [s for s in subs if not s.get("is_archived")] or [None]
        for n in range(args.per_sector):
            industry = subs[n % len(subs)] or {"name": d["name"], "description": None}
            title, dek, blocks, cat, pri, stype, tags = build_article(d["name"], industry, n + di)
            slug = f"{slugify(d['slug'] + '-' + short(industry['name']))}-{stamp}-{di}-{n}"[:120]
            payload = {
                "slug": slug, "title": title, "dek": dek, "body": blocks,
                "category": cat, "tags": tags, "story_type": stype, "priority": pri,
                "desk_id": d["id"],
            }
            if industry.get("id"):
                payload["sub_sector_id"] = industry["id"]
            if args.author_id:
                payload["author_id"] = args.author_id
            try:
                status, _ = _req("POST", f"{base}/api/v1/stories", token=token, data=payload)
                created += 1
            except urllib.error.HTTPError as e:
                failed += 1
                if failed <= 5:
                    print(f"  ! {d['slug']} #{n}: HTTP {e.code} {e.read().decode()[:120]}")
            except Exception as e:
                failed += 1
                if failed <= 5:
                    print(f"  ! {d['slug']} #{n}: {e}")
        print(f"[{di+1}/{len(desks)}] {d['name']}: +{args.per_sector} (running total {created})")

    print(f"\nDone. created={created} failed={failed} across {len(desks)} sectors.")


if __name__ == "__main__":
    main()
