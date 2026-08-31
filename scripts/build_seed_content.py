#!/usr/bin/env python3
"""Rebuilds the content half of `seed-igennews.sql` from the long-form article set.

The people, memberships and role assignments at the top of that file are good and
are copied through untouched. Everything from the stories block onward is
regenerated, because the old content had two problems this fixes:

  * articles averaged 163 words, so no page could demonstrate long-form reading;
  * stories were marked `published` with no version, review, approval, workflow
    history or release behind them — the publishing queue and corrections ledger
    were therefore always empty, and the editorial trail the product promises did
    not exist. See SEED-GAPS.md.

Usage:
    python3 build_seed_content.py            # writes seed-igennews.sql in place
    python3 build_seed_content.py --check    # validate inputs only, write nothing
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import random
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SEED = os.path.join(HERE, "seed-igennews.sql")
ARTICLES = os.path.join(HERE, "articles")

# Deterministic output: regenerating the seed twice from the same articles must
# produce the same file, or every rebuild is an unreviewable diff.
RNG = random.Random(20260831)

# ── the cast ───────────────────────────────────────────────────────────────
# Sign-off independence is enforced by the application: an author may not decide
# a review of their own story. These three reviewers are therefore never authors.
COPY_EDITOR = "s.banerjee"
FACT_CHECKER = "r.pillai"
MANAGING_EDITOR = "v.iyer"

# Which reporter and section editor own each sector. The first 18 match the beats
# the previous seed already established; the rest extend the same pattern so all
# 50 sectors have an owner rather than 32 of them leading nowhere.
BEATS = {
    "agriculture": ("r.agri", "e.agri"),
    "ai-and-cyber-security": ("r.ai", "e.tech"),
    "animal-husbandry-dairying-and-fisheries": ("r.agri", "e.agri"),
    "atomic-energy": ("r.energy", "e.energy"),
    "automotive-and-electric-vehicles": ("r.auto", "e.auto"),
    "ayush-and-ayurveda-and-herbal-medicine": ("r.health", "e.health"),
    "banking-and-financial-services": ("r.bank", "e.fin"),
    "biotechnology": ("r.pharma", "e.health"),
    "chemicals-and-fertilizers-and-minerals": ("r.energy", "e.energy"),
    "civil-aviation": ("r.infra", "e.infra"),
    "coal": ("r.energy", "e.energy"),
    "communications": ("r.tech", "e.tech"),
    "consumer-brands": ("r.retail", "e.biz"),
    "defence-and-aerospace": ("r.def", "e.def"),
    "earth-sciences": ("r.renew", "e.energy"),
    "education": ("r.startup", "e.biz"),
    "electronics-and-it-and-components": ("r.chip", "e.tech"),
    "energy-and-sustainability": ("r.energy", "e.energy"),
    "environment-forest-and-climate-change": ("r.renew", "e.energy"),
    "fertilizers": ("r.agri", "e.agri"),
    "fintech-and-digital-payments": ("r.fintech", "e.fin"),
    "fisheries-animal-husbandry-and-dairying": ("r.agri", "e.agri"),
    "fmcg": ("r.retail", "e.biz"),
    "food-processing-industries": ("r.agri", "e.agri"),
    "health-and-family-welfare": ("r.health", "e.health"),
    "heavy-industries": ("r.infra", "e.infra"),
    "information-and-broadcasting": ("r.tech", "e.tech"),
    "infrastructure-and-construction": ("r.infra", "e.infra"),
    "labour-and-employment": ("r.startup", "e.biz"),
    "logistics-and-supply-chain": ("r.retail", "e.biz"),
    "manufacturing": ("r.infra", "e.infra"),
    "mines": ("r.energy", "e.energy"),
    "new-and-renewable-energy": ("r.renew", "e.energy"),
    "petrochemicals": ("r.energy", "e.energy"),
    "petroleum-and-natural-gas": ("r.energy", "e.energy"),
    "pharmaceutical": ("r.pharma", "e.health"),
    "ports-shipping-and-waterways": ("r.infra", "e.infra"),
    "power": ("r.energy", "e.energy"),
    "railways-and-metro": ("r.rail", "e.infra"),
    "retail-and-e-commerce": ("r.retail", "e.biz"),
    "semiconductors": ("r.chip", "e.tech"),
    "services": ("r.startup", "e.biz"),
    "space": ("r.space", "e.def"),
    "startups-and-innovation": ("r.startup", "e.fin"),
    "steel": ("r.infra", "e.infra"),
    "technology": ("r.tech", "e.tech"),
    "textiles": ("r.retail", "e.biz"),
    "tourism": ("r.tourism", "e.biz"),
    "waste-management-and-circular-economy": ("r.renew", "e.energy"),
    "water-resources-and-management": ("r.renew", "e.energy"),
}


# Desk slugs referenced by the hand-written governance fixtures below. They are
# validated against subsectors.json before anything is emitted, because a desk
# slug that does not exist does not fail — it disappears through the JOIN and
# leaves the fixture silently half-seeded. Four rows were lost that way before
# this check existed.
FIXTURE_DESKS = {
    "finance": "banking-and-financial-services",
    "tech": "technology",
    "health": "health-and-family-welfare",
    "climate": "environment-forest-and-climate-change",
    "logistics": "logistics-and-supply-chain",
    "defence": "defence-and-aerospace",
    "aicyber": "ai-and-cyber-security",
}


def q(text: str) -> str:
    """Quote a value for a SQL single-quoted literal."""
    return text.replace("'", "''")


def words(article: dict) -> int:
    return sum(len(b.get("text", "").split()) for b in article["body"])


def load_articles() -> list[dict]:
    files = sorted(glob.glob(os.path.join(ARTICLES, "articles-*.json")))
    if not files:
        sys.exit(f"no article files in {ARTICLES}")
    out: list[dict] = []
    for f in files:
        out.extend(json.load(open(f, encoding="utf-8")))
    return out


def validate(articles: list[dict], subsectors: dict) -> list[str]:
    """Refuse to build a seed from content that would reintroduce known defects."""
    problems: list[str] = []
    # A fixture desk slug that no longer exists JOINs to nothing and silently
    # drops its whole row, so it is checked before any content is looked at.
    for key, desk_slug in FIXTURE_DESKS.items():
        if desk_slug not in subsectors:
            problems.append(f"FIXTURE_DESKS[{key!r}]: no such sector {desk_slug!r}")
    seen: set[str] = set()
    for a in articles:
        slug = a.get("slug", "?")
        if slug in seen:
            problems.append(f"duplicate slug: {slug}")
        seen.add(slug)
        if a["sector_slug"] not in BEATS:
            problems.append(f"{slug}: no beat owner for sector {a['sector_slug']}")
        if a["sector_slug"] not in subsectors:
            problems.append(f"{slug}: unknown sector {a['sector_slug']}")
        body = a.get("body") or []
        if not body:
            problems.append(f"{slug}: empty body")
            continue
        if body[0].get("type") != "paragraph":
            problems.append(f"{slug}: body starts with {body[0].get('type')}, not paragraph")
        # The defect this whole rebuild exists to remove.
        for b in body:
            if b.get("type") == "heading" and b.get("text", "").strip() == a["title"].strip():
                problems.append(f"{slug}: heading duplicates the title")
        n = words(a)
        if not 500 <= n <= 1600:
            problems.append(f"{slug}: {n} words, outside 500-1600")
    return problems


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true", help="validate only")
    args = ap.parse_args()

    subsectors = json.load(open(os.path.join(HERE, "subsectors.json"), encoding="utf-8"))
    articles = load_articles()

    problems = validate(articles, subsectors)
    if problems:
        print(f"{len(problems)} problem(s):")
        for p in problems[:25]:
            print("  -", p)
        sys.exit(1)

    counts = sorted(words(a) for a in articles)
    print(f"  articles: {len(articles)} across {len({a['sector_slug'] for a in articles})} sectors")
    print(f"  words: min {counts[0]}, median {counts[len(counts)//2]}, max {counts[-1]}")
    if args.check:
        return

    # ── assign workflow state and age ──────────────────────────────────────
    # Most of the archive is published; a minority is deliberately left in flight
    # so the review queue, publishing queue and task board are not empty either.
    plan = []
    for i, a in enumerate(articles):
        author, editor = BEATS[a["sector_slug"]]
        subs = subsectors[a["sector_slug"]]
        bucket = i % 10
        # `workflow_state` has no `published` member: the pipeline ends at
        # `ready` and publication is recorded separately on `publication_state`.
        # Unpublished work is `not_live`, not `draft`.
        if bucket <= 6:
            wf, pub = "ready", "published"
        elif bucket == 7:
            wf, pub = "ready", "not_live"
        elif bucket == 8:
            wf, pub = "copy_standards", "not_live"
        else:
            wf, pub = "drafting", "not_live"
        plan.append({
            "a": a,
            "author": author,
            "editor": editor,
            "wf": wf,
            "pub": pub,
            "age_h": 6 + i * 11,                       # ~2 months, newest first
            "due_days": None if pub == "published" else (i % 9) - 2,
            "ind": subs[i % len(subs)] if subs else None,
        })

    sql = build_sql(plan)
    original = open(SEED, encoding="utf-8").read()
    head = original.split("-- Stories:")[0]
    out = head + sql

    # The seed documents itself as "a clean reset", and the deploy runbook tells
    # operators to re-run it. That only holds if every table written here is also
    # cleared by the preamble — otherwise the second run dies on a primary-key
    # collision partway through, which is exactly what happened for four tables.
    # `users` is exempt: the preamble clears it with DELETE, not TRUNCATE.
    reset = set()
    m = re.search(r"TRUNCATE\s+(.*?)RESTART IDENTITY CASCADE;", head, re.S)
    if m:
        reset = {t.strip() for t in m.group(1).replace("\n", " ").split(",") if t.strip()}
    reset |= set(re.findall(r"DELETE FROM\s+([a-z_]+)", head))
    written = set(re.findall(r"INSERT INTO\s+([a-z_]+)", out))
    unreset = sorted(written - reset)
    if unreset:
        print("  seed would not be re-runnable; not cleared by the preamble:")
        for t in unreset:
            print("   -", t)
        sys.exit(1)

    open(SEED, "w", encoding="utf-8").write(out)
    print(f"  wrote {SEED} ({len(written)} tables, all re-runnable)")


def build_sql(plan: list[dict]) -> str:
    """Emit the stories block, its dependants, and the editorial trail."""
    out: list[str] = []
    w = out.append

    # ── stories ────────────────────────────────────────────────────────────
    w("-- Stories: long-form bodies across all 50 sectors, most published.")
    w("INSERT INTO stories")
    w("  (id, slug, title, dek, body, category, workflow_state, publication_state, priority,")
    w("   desk_id, sub_sector_id, author_id, editor_id, fact_checker_id, due_at, published_at, created_at, updated_at)")
    w("SELECT gen_random_uuid(), v.slug, v.title, v.dek, v.body::jsonb, v.category, v.wf, v.pub, v.pri,")
    w("  d.id, ss.id, au.id, ed.id, fc.id,")
    w("  CASE WHEN v.due_days IS NULL THEN NULL ELSE now() + make_interval(days => v.due_days) END,")
    w("  CASE WHEN v.pub='published' THEN now() - make_interval(hours => v.age_h) ELSE NULL END,")
    w("  now() - make_interval(hours => v.age_h), now() - make_interval(hours => (v.age_h/2))")
    w("FROM (VALUES")
    rows = []
    for p in plan:
        a = p["a"]
        body = json.dumps(a["body"], ensure_ascii=False)
        ind = f"'{q(p['ind'])}'" if p["ind"] else "NULL"
        due = "NULL" if p["due_days"] is None else str(p["due_days"])
        rows.append(
            f"  ('{q(a['sector_slug'])}', '{q(a['slug'])}', '{q(a['title'])}', '{q(a['dek'])}', "
            f"'{q(body)}', '{q(a['category'])}', '{p['wf']}', '{p['pub']}', '{a['priority']}', "
            f"'{p['author']}', '{p['editor']}', '{FACT_CHECKER}', {due}, {p['age_h']}, {ind})"
        )
    w(",\n".join(rows))
    w(") AS v(sector_slug, slug, title, dek, body, category, wf, pub, pri, author_handle, editor_handle, fc_handle, due_days, age_h, ind_slug)")
    w("JOIN desks d ON d.slug=v.sector_slug JOIN users au ON au.handle=v.author_handle JOIN users ed ON ed.handle=v.editor_handle")
    w("LEFT JOIN users fc ON fc.handle=v.fc_handle LEFT JOIN sub_sectors ss ON ss.desk_id=d.id AND ss.slug=v.ind_slug;")
    w("")

    published = [p for p in plan if p["pub"] == "published"]
    inflight = [p for p in plan if p["pub"] != "published"]

    # ── version 1 for every story ──────────────────────────────────────────
    # Reviews attach to a version, so without this nothing can be reviewed and
    # the entire sign-off chain is unreachable.
    w("-- Every story has a filed version 1; reviews and approvals attach to it.")
    w("INSERT INTO story_versions (id, story_id, number, title, dek, body, category, tags, change_summary, created_by_id, filed, created_at)")
    w("SELECT gen_random_uuid(), s.id, 1, s.title, s.dek, s.body, s.category, '[]'::jsonb,")
    w("  'Filed for desk review.', s.author_id, true, s.created_at + interval '2 hours'")
    w("FROM stories s;")
    w("")

    # ── reviews + approvals for published work ─────────────────────────────
    # Three kinds, three different people, none of them the author: the product
    # refuses a sign-off from a story's own author.
    w("-- The three required sign-offs on published work, from three different people.")
    w("-- None is the author: an author may not decide a review of their own story.")
    w("INSERT INTO reviews (id, story_id, version_id, kind, assigned_to_id, requested_by_id, decision, notes, decided_at, created_at, updated_at)")
    w("SELECT gen_random_uuid(), s.id, sv.id, k.kind, rv.id, s.editor_id, 'approved', k.note,")
    w("  s.published_at - interval '3 hours', s.created_at + interval '4 hours', s.published_at - interval '3 hours'")
    w("FROM stories s")
    w("JOIN story_versions sv ON sv.story_id = s.id AND sv.number = 1")
    w("CROSS JOIN (VALUES")
    w(f"    ('copy', 'Style and house usage checked.', '{COPY_EDITOR}'),")
    w("    ('desk', 'Angle and framing agreed with the desk.', NULL),")
    w(f"    ('fact_check', 'Claims verified against sourcing.', '{FACT_CHECKER}')")
    w("  ) AS k(kind, note, handle)")
    w("JOIN users rv ON rv.handle = COALESCE(k.handle, (SELECT handle FROM users u WHERE u.id = s.editor_id))")
    w("WHERE s.publication_state = 'published';")
    w("")
    w("INSERT INTO approvals (id, story_id, version_id, review_id, kind, approver_id, decision, rationale, is_valid, created_at)")
    w("SELECT gen_random_uuid(), r.story_id, r.version_id, r.id, r.kind, r.assigned_to_id, 'approved',")
    w("  r.notes, true, r.decided_at")
    w("FROM reviews r WHERE r.decision = 'approved';")
    w("")

    # ── a pending review on in-flight work ─────────────────────────────────
    w("-- In-flight stories carry a genuinely pending review, so the review queue")
    w("-- and the attention feed have something real to show.")
    w("INSERT INTO reviews (id, story_id, version_id, kind, assigned_to_id, requested_by_id, decision, notes, created_at, updated_at)")
    w("SELECT gen_random_uuid(), s.id, sv.id, 'desk', s.editor_id, s.author_id, 'pending', NULL,")
    w("  s.updated_at, s.updated_at")
    w("FROM stories s JOIN story_versions sv ON sv.story_id = s.id AND sv.number = 1")
    # Published work also sits at `ready` — publication is a separate axis — so it
    # must be excluded here, or every published story would carry a review that is
    # still waiting on a decision it already received.
    w("WHERE s.workflow_state IN ('ready', 'copy_standards') AND s.publication_state <> 'published';")
    w("")

    # ── workflow history ───────────────────────────────────────────────────
    w("-- The state history the audit trail promises. Published work walks the whole")
    w("-- pipeline; in-flight work stops where it actually is.")
    # `workflow_state_history_open_uq` is UNIQUE (story_id) WHERE exited_at IS NULL:
    # exactly one row may be open, and it is the story's current state. Every
    # earlier step must therefore be closed at the moment the next one began.
    w("INSERT INTO workflow_state_history (id, story_id, from_state, to_state, actor_id, entered_at, exited_at, reason)")
    w("SELECT gen_random_uuid(), s.id, t.from_state, t.to_state, s.author_id,")
    w("  s.created_at + (t.step * interval '3 hours'),")
    w("  CASE WHEN t.step < 6 THEN s.created_at + ((t.step + 1) * interval '3 hours') ELSE NULL END,")
    w("  t.reason")
    w("FROM stories s CROSS JOIN (VALUES")
    w("    (1, 'proposed', 'reporting', 'Commissioned.'),")
    w("    (2, 'reporting', 'drafting', 'Reporting complete.'),")
    w("    (3, 'drafting', 'desk_review', 'Filed to the desk.'),")
    w("    (4, 'desk_review', 'verification', 'Desk cleared.'),")
    w("    (5, 'verification', 'copy_standards', 'Claims verified.'),")
    w("    (6, 'copy_standards', 'ready', 'Copy and standards cleared.')")
    w("  ) AS t(step, from_state, to_state, reason)")
    w("WHERE s.publication_state = 'published';")
    w("")

    # ── releases ───────────────────────────────────────────────────────────
    w("-- A publication record for every published story. Without these the")
    w("-- publishing queue is empty even though the archive says 'published'.")
    w("INSERT INTO releases (id, story_id, version_id, release_type, status, channels, receipts, published_at, created_by_id, created_at)")
    w("SELECT gen_random_uuid(), s.id, sv.id, 'publish', 'published',")
    w("  '[{\"name\": \"web\"}]'::jsonb,")
    w("  jsonb_build_array(jsonb_build_object('channel','web','status','delivered','at', to_char(s.published_at, 'YYYY-MM-DD\"T\"HH24:MI:SSZ'))),")
    w("  s.published_at, s.editor_id, s.published_at - interval '1 hour'")
    w("FROM stories s JOIN story_versions sv ON sv.story_id = s.id AND sv.number = 1")
    w("WHERE s.publication_state = 'published';")
    w("")
    w("INSERT INTO release_attempts (id, release_id, attempt_number, status, trigger, started_at, completed_at)")
    w("SELECT gen_random_uuid(), r.id, 1, 'succeeded', 'worker',")
    w("  r.published_at - interval '2 minutes', r.published_at")
    w("FROM releases r;")
    w("")

    # ── a scheduled release, so the queue has something pending ────────────
    w("-- One scheduled release so the publishing queue shows pending work, not")
    w("-- only history.")
    w("INSERT INTO releases (id, story_id, version_id, release_type, status, channels, receipts, scheduled_at, created_by_id, created_at)")
    w("SELECT gen_random_uuid(), s.id, sv.id, 'publish', 'scheduled', '[{\"name\": \"web\"}]'::jsonb, '[]'::jsonb,")
    w("  now() + interval '6 hours', s.editor_id, now() - interval '2 hours'")
    w("FROM stories s JOIN story_versions sv ON sv.story_id = s.id AND sv.number = 1")
    w("WHERE s.workflow_state = 'ready' LIMIT 3;")
    w("")

    # ── corrections ────────────────────────────────────────────────────────
    corrected = [p["a"]["slug"] for p in published[:4]]
    w("-- Corrections against published releases, so the ledger and the article")
    w("-- footer have real entries: two resolved, one still open.")
    w("INSERT INTO corrections (id, story_id, release_id, reported_by_id, owner_id, classification, status, description, public_note, resolved_at, created_at, updated_at)")
    w("SELECT gen_random_uuid(), s.id, r.id, rep.id, s.editor_id, v.classification, v.status, v.description,")
    w("  v.public_note,")
    w("  CASE WHEN v.status='resolved' THEN s.published_at + interval '2 days' ELSE NULL END,")
    w("  s.published_at + interval '1 day', s.published_at + interval '2 days'")
    w("FROM (VALUES")
    w(f"    ('{q(corrected[0])}', 'correction', 'resolved', 'A production figure in paragraph four was overstated.', 'An earlier version overstated the production figure. It has been corrected.'),")
    w(f"    ('{q(corrected[1])}', 'clarification', 'resolved', 'The scope of the policy was ambiguous as written.', 'This article was updated to clarify which categories the policy covers.'),")
    w(f"    ('{q(corrected[2])}', 'correction', 'open', 'A named location may be incorrect; reporter is checking.', NULL)")
    w("  ) AS v(slug, classification, status, description, public_note)")
    w("JOIN stories s ON s.slug = v.slug")
    w("JOIN releases r ON r.story_id = s.id AND r.status = 'published'")
    w("CROSS JOIN (SELECT id FROM users WHERE handle='n.desai') rep;")
    w("")

    # ── front page ─────────────────────────────────────────────────────────
    w("-- Front page: the newest published work.")
    w("INSERT INTO front_page_slots (id, position, label, story_id, is_pinned, updated_by_id, updated_at)")
    w("SELECT gen_random_uuid(), v.position, v.label, s.id, v.pinned, a.id, now() FROM (VALUES")
    labels = ["Lead", "Second lead", "Analysis", "Feature", "Also today", "Watching"]
    fp = []
    for i, label in enumerate(labels):
        fp.append(f"  ({i + 1}, '{label}', '{q(published[i]['a']['slug'])}', {'true' if i == 0 else 'false'})")
    w(",\n".join(fp))
    w(") AS v(position, label, slug, pinned) JOIN stories s ON s.slug=v.slug CROSS JOIN (SELECT id FROM users WHERE handle='admin') a;")
    w("")

    # ── feed ───────────────────────────────────────────────────────────────
    w("-- Newsroom feed: posts about recent published work, with engagement.")
    w("INSERT INTO feed_posts (id, author_id, kind, content, story_id, created_at, updated_at)")
    w("SELECT gen_random_uuid(), u.id, v.kind, v.body, s.id, now() - make_interval(hours => v.age_h), now() - make_interval(hours => v.age_h) FROM (VALUES")
    openers = [
        ("link", "Published today: {t}"),
        ("link", "New from the desk: {t}"),
        ("note", "Weeks of reporting behind this one. {t}"),
        ("link", "Our latest on the beat: {t}"),
        ("note", "Worth your time this morning: {t}"),
        ("link", "Live now: {t}"),
        ("quote", "“The fundamentals are strong; the next year decides who leads.” — from our reporting on {t}"),
        ("link", "If you read one thing today: {t}"),
        ("note", "Some context behind the numbers in {t}"),
        ("link", "Now up: {t}"),
        ("link", "From the sector desk: {t}"),
        ("note", "A quieter story than the headlines suggest: {t}"),
    ]
    posts = []
    for i, (kind, tpl) in enumerate(openers):
        p = published[i]
        title = p["a"]["title"]
        posts.append(
            f"  ('{p['author']}', '{kind}', '{q(tpl.format(t=title))}', '{q(p['a']['slug'])}', {8 + i * 9})"
        )
    w(",\n".join(posts))
    w(") AS v(handle, kind, body, story_slug, age_h) JOIN users u ON u.handle=v.handle LEFT JOIN stories s ON s.slug=v.story_slug;")
    w("")
    w("-- Engagement, so the counts on a post are not all zero.")
    w("INSERT INTO feed_likes (post_id, user_id, created_at)")
    w("SELECT p.id, u.id, p.created_at + interval '20 minutes'")
    w("FROM feed_posts p CROSS JOIN LATERAL (")
    w("  SELECT id FROM users WHERE role IN ('reporter','section_editor') ORDER BY md5(id::text || p.id::text) LIMIT 3")
    w(") u ON CONFLICT DO NOTHING;")
    w("UPDATE feed_posts p SET likes = (SELECT count(*) FROM feed_likes l WHERE l.post_id = p.id);")
    w("")
    w("INSERT INTO feed_replies (id, post_id, author_id, body, created_at)")
    w("SELECT gen_random_uuid(), p.id, u.id, v.body, p.created_at + interval '45 minutes'")
    w("FROM feed_posts p")
    w("JOIN LATERAL (SELECT id FROM users WHERE handle='e.biz') u ON true")
    w("CROSS JOIN (VALUES ('Strong piece. The supply-chain section is the part to watch.')) AS v(body)")
    w("WHERE p.kind = 'link' AND p.created_at < now() - interval '30 hours';")
    w("UPDATE feed_posts p SET replies = (SELECT count(*) FROM feed_replies r WHERE r.post_id = p.id);")
    w("")

    # ── tasks ──────────────────────────────────────────────────────────────
    w("-- Tasks against in-flight stories.")
    w("INSERT INTO tasks (id, story_id, desk_id, title, description, status, priority, assigned_to_id, created_by_id, due_at, created_at, updated_at)")
    w("SELECT gen_random_uuid(), s.id, d.id, v.title, v.description, v.status, v.priority, asg.id, cr.id,")
    w("  now() + make_interval(days => v.due_days), now() - interval '3 days', now() - interval '1 day'")
    w("FROM (VALUES")
    task_titles = [
        ("Second source on the central claim", "The desk wants a second, independent source before this runs.", "in_progress", "high", 1),
        ("Confirm the plant figures", "Numbers in the third section need confirming against the filing.", "todo", "medium", 2),
        ("Add regional context", "Reads as national; needs a state-level angle.", "todo", "medium", 4),
        ("Standards read", "Legal and standards pass before scheduling.", "todo", "high", 1),
        ("Chase the ministry response", "Right of reply requested; no answer yet.", "in_progress", "urgent", -1),
        ("Cut to length", "Overlong for the slot by roughly two hundred words.", "todo", "low", 3),
    ]
    trows = []
    for i, (title, desc, status, pri, due) in enumerate(task_titles):
        p = inflight[i % len(inflight)]
        trows.append(
            f"  ('{q(p['a']['slug'])}', '{q(p['a']['sector_slug'])}', '{q(title)}', '{q(desc)}', "
            f"'{status}', '{pri}', '{p['author']}', '{p['editor']}', {due})"
        )
    w(",\n".join(trows))
    w(") AS v(story_slug, sector_slug, title, description, status, priority, assignee, creator, due_days)")
    w("JOIN stories s ON s.slug=v.story_slug JOIN desks d ON d.slug=v.sector_slug")
    w("JOIN users asg ON asg.handle=v.assignee JOIN users cr ON cr.handle=v.creator;")
    w("")

    build_newsroom_extras(w, plan, published, inflight)
    build_governance_extras(w, plan, published, inflight)
    w("COMMIT;")
    return "\n".join(out) + "\n"


def build_newsroom_extras(w, plan, published, inflight) -> None:
    """The surfaces the old seed left empty: sourcing, media, desk configuration
    and the personal state behind visible controls.

    Values here are checked against the schema's own constraints rather than
    guessed — `sources.ground_rule`, `claims.status`, `pitches.status`,
    `legal_documents.status` and `sector_applications.status` are all closed sets,
    and the previous attempt at this seed failed on exactly that kind of mistake.
    """

    # ── the pipeline that feeds the archive ────────────────────────────────
    # Restored: the rebuild replaced everything after the stories block, and these
    # two lived below it.
    w("-- Pitches in every state the board can show, with the money terms a")
    w("-- contributor proposes (currency must match ^[A-Z]{3}$; minor units are integers).")
    w("INSERT INTO pitches (id, headline, summary, angle, desk_id, created_by_id, assignee_id, editor_id,")
    w("  status, priority, key_questions, likely_sources, risks, currency, fee_minor, expense_minor,")
    w("  target_at, created_at, updated_at)")
    w("SELECT gen_random_uuid(), v.headline, v.summary, v.angle, d.id, cr.id, asg.id, ed.id,")
    w("  v.status, v.priority, v.key_questions::jsonb, v.likely_sources::jsonb, v.risks::jsonb,")
    w("  'INR', v.fee, v.expense,")
    w("  now() + make_interval(days => v.target_days), now() - make_interval(days => v.age_d), now() - make_interval(days => (v.age_d/2))")
    w("FROM (VALUES")
    pitches = [
        ("Who actually pays for the grid upgrade", "State discoms and private developers disagree over who funds evacuation infrastructure.",
         "Follow the cost, not the announcement.", "energy-and-sustainability", "r.energy", "e.energy",
         "commissioned", "high", 45000000, 1200000, 12, 26),
        ("The quiet consolidation in diagnostics", "Three regional chains have changed hands in eight months.",
         "Who is buying, and what happens to prices.", "health-and-family-welfare", "r.health", "e.health",
         "proposed", "medium", 30000000, 800000, 20, 18),
        ("What the new tariff order means for small exporters", "The order reads as relief; exporters say the thresholds exclude them.",
         "Read the schedule, not the press note.", "textiles", "r.retail", "e.biz",
         "needs_detail", "medium", 25000000, 500000, 15, 11),
        ("Inside a semiconductor fab that has not opened", "Two years after the groundbreaking, the site employs a security detail.",
         "A visit, not a briefing.", "semiconductors", "r.chip", "e.tech",
         "commissioned", "urgent", 60000000, 3500000, 8, 31),
        ("The metro line that changed a suburb's rents", "Property listings within a kilometre of the corridor tell their own story.",
         "Ground reporting plus listing data.", "railways-and-metro", "r.rail", "e.infra",
         "proposed", "low", 20000000, 600000, 30, 9),
        ("Why fertiliser subsidy claims keep getting rejected", "District officers and manufacturers describe the same process differently.",
         "Both sides of one paperwork trail.", "fertilizers", "r.agri", "e.agri",
         "parked", "low", 18000000, 400000, 40, 44),
        ("A defence supplier's first export order", "The order is small; the precedent is not.",
         "What it took to qualify.", "defence-and-aerospace", "r.def", "e.def",
         "declined", "medium", 22000000, 900000, 25, 52),
        ("The cold chain gap between the field and the mandi", "Losses concentrate in a stretch nobody owns.",
         "Follow one consignment end to end.", "food-processing-industries", "r.agri", "e.agri",
         "commissioned", "high", 35000000, 2100000, 10, 21),
    ]
    rows = []
    for (h, summ, ang, desk, rep, ed, st, pri, fee, exp, target, age) in pitches:
        kq = '["Who benefits?", "What does the paperwork actually say?", "Who is not being asked?"]'
        ls = '["A district officer", "Two manufacturers", "A trade body"]'
        rk = '["Access may be refused", "Figures may not be disclosable"]'
        rows.append(
            f"  ('{q(h)}', '{q(summ)}', '{q(ang)}', '{desk}', '{rep}', '{rep}', '{ed}', "
            f"'{st}', '{pri}', '{q(kq)}', '{q(ls)}', '{q(rk)}', {fee}, {exp}, {target}, {age})"
        )
    w(",\n".join(rows))
    w(") AS v(headline, summary, angle, sector_slug, reporter, assignee, editor, status, priority,")
    w("      key_questions, likely_sources, risks, fee, expense, target_days, age_d)")
    w("JOIN desks d ON d.slug=v.sector_slug JOIN users cr ON cr.handle=v.reporter")
    w("JOIN users asg ON asg.handle=v.assignee JOIN users ed ON ed.handle=v.editor;")
    w("")

    w("-- Coverage diary: what the desks are planning to cover.")
    w("INSERT INTO coverage_events (id, title, description, desk_id, owner_id, starts_at, ends_at, status, location, created_at, updated_at)")
    w("SELECT gen_random_uuid(), v.title, v.description, d.id, o.id,")
    w("  now() + make_interval(days => v.in_days), now() + make_interval(days => v.in_days, hours => 4),")
    w("  v.status, v.location, now() - interval '10 days', now() - interval '2 days'")
    w("FROM (VALUES")
    events = [
        ("Union Budget: sector reaction", "Reaction and analysis across the desks on budget day.", "banking-and-financial-services", "r.bank", 9, "planned", "New Delhi"),
        ("Auto Expo press days", "Launches, order books and supplier conversations.", "automotive-and-electric-vehicles", "r.auto", 21, "planned", "Greater Noida"),
        ("Grid regulator open hearing", "Tariff petition hearing, open to press.", "power", "r.energy", 5, "planned", "New Delhi"),
        ("Semiconductor policy briefing", "Ministry briefing on incentive disbursal.", "semiconductors", "r.chip", 14, "planned", "Bengaluru"),
        ("Kharif sowing review", "State-wise sowing numbers with the ministry.", "agriculture", "r.agri", 3, "planned", "New Delhi"),
        ("Port capacity site visit", "Berth expansion walkthrough with the authority.", "ports-shipping-and-waterways", "r.infra", 30, "planned", "Mundra"),
        ("Pharma export council meet", "Quarterly export figures and compliance updates.", "pharmaceutical", "r.pharma", 17, "planned", "Hyderabad"),
        ("Launch window briefing", "Mission profile and payload manifest.", "space", "r.space", 26, "planned", "Sriharikota"),
        ("Textile buyers' forum", "Order pipeline and tariff impact.", "textiles", "r.retail", 12, "cancelled", "Tiruppur"),
    ]
    w(",\n".join(
        f"  ('{q(t)}', '{q(desc)}', '{desk}', '{owner}', {days}, '{st}', '{q(loc)}')"
        for (t, desc, desk, owner, days, st, loc) in events))
    w(") AS v(title, description, sector_slug, owner, in_days, status, location)")
    w("JOIN desks d ON d.slug=v.sector_slug JOIN users o ON o.handle=v.owner;")
    w("")

    # ── sourcing and verification ──────────────────────────────────────────
    # Attached to a slice of published work rather than all of it: a newsroom
    # where every story carries identical sourcing reads as generated.
    w("-- Sourcing on a slice of the published archive. `ground_rule` is a closed")
    w("-- set: on_record | background | deep_background | off_record.")
    w("INSERT INTO sources (id, story_id, identity, publishable_attribution, ground_rule, description, reliability_notes, access_level, manager_approved_by_id, approval_rationale, created_by_id, created_at, updated_at)")
    w("SELECT gen_random_uuid(), s.id, v.identity, v.attribution, v.ground_rule, v.description,")
    w("  v.reliability,")
    # 'story_team' is the column's own schema default and the narrower of the two
    # values in evidence; nothing in the codebase reads access_level yet, so the
    # seed must not invent a third vocabulary term for it.
    w("  CASE WHEN v.ground_rule IN ('deep_background','off_record') THEN 'story_team' ELSE 'desk' END,")
    w("  mgr.id, 'Identity held by the desk; attribution agreed.', s.author_id,")
    w("  s.created_at + interval '1 hour', s.created_at + interval '1 hour'")
    w("FROM stories s")
    w("JOIN LATERAL (VALUES")
    w("    ('Senior official, sector ministry', 'a senior official with knowledge of the file', 'background',")
    w("     'Sighted the draft order before circulation.', 'Reliable on process; cautious on numbers.'),")
    w("    ('Plant-level manager', 'a manager at the facility', 'on_record',")
    w("     'Walked the line and answered on the record.', 'First-hand and consistent with documents.'),")
    w("    ('Trade body analyst', 'an analyst tracking the sector', 'on_record',")
    w("     'Provided the published series behind the trend.', 'Published work, checkable.'),")
    w("    ('Contract auditor on the programme', 'a person with direct knowledge of the audit', 'deep_background',")
    w("     'Described the findings; will not be quoted or characterised by role.',")
    w("     'Two documents seen corroborate the account.'),")
    w("    ('Former employee of the contracting firm', 'not attributable', 'off_record',")
    w("     'Steered the reporting; nothing in the piece rests on this account alone.',")
    w("     'Motivated but accurate on dates and names.')")
    w("  ) AS v(identity, attribution, ground_rule, description, reliability) ON true")
    w("CROSS JOIN (SELECT id FROM users WHERE handle='v.iyer') mgr")
    w("WHERE s.publication_state = 'published'")
    w("  AND (('x' || substr(md5(s.slug), 1, 8))::bit(32)::int % 3) = 0;")
    w("")

    w("-- Evidence, tied to the source that supplied it.")
    w("INSERT INTO evidence (id, story_id, source_id, kind, title, uri, notes, verification_status, created_by_id, acquired_at, created_at, updated_at)")
    w("SELECT gen_random_uuid(), src.story_id, src.id, v.kind, v.title, v.uri, v.notes, 'verified',")
    w("  src.created_by_id, src.created_at, src.created_at, src.created_at")
    w("FROM sources src")
    w("JOIN LATERAL (VALUES")
    w("    ('document', 'Draft order, as circulated', NULL, 'Held by the desk; not for publication.'),")
    w("    ('dataset', 'Quarterly series behind the trend line', NULL, 'Published source, recomputed independently.')")
    w("  ) AS v(kind, title, uri, notes) ON true")
    w("WHERE src.ground_rule <> 'off_record';")
    w("")

    w("-- Claims checked against that evidence. `status` is the same closed set the")
    w("-- assets and evidence tables use.")
    w("INSERT INTO claims (id, story_id, version_id, text, locator, status, reviewed_by_id, decision_note, reviewed_at, created_at, updated_at)")
    w("SELECT gen_random_uuid(), s.id, sv.id, v.text, v.locator, 'verified', fc.id,")
    w("  'Checked against the filed evidence.', s.published_at - interval '4 hours',")
    w("  s.created_at + interval '5 hours', s.published_at - interval '4 hours'")
    w("FROM stories s")
    w("JOIN story_versions sv ON sv.story_id = s.id AND sv.number = 1")
    w("JOIN LATERAL (VALUES")
    w("    ('The growth figure cited in the opening section.', 'para:2'),")
    w("    ('The attribution on the central quote.', 'para:5')")
    w("  ) AS v(text, locator) ON true")
    w(f"CROSS JOIN (SELECT id FROM users WHERE handle='{FACT_CHECKER}') fc")
    w("WHERE s.publication_state = 'published'")
    w("  AND (('x' || substr(md5(s.slug), 1, 8))::bit(32)::int % 3) = 0;")
    w("")

    # ── desk conversation ──────────────────────────────────────────────────
    w("-- Desk comments on in-flight drafts, some resolved.")
    w("INSERT INTO comments (id, story_id, version_id, author_id, body, locator, resolved, resolved_by_id, resolved_at, created_at, updated_at)")
    w("SELECT gen_random_uuid(), s.id, sv.id, s.editor_id, v.body, v.locator, v.resolved,")
    w("  CASE WHEN v.resolved THEN s.author_id ELSE NULL END,")
    w("  CASE WHEN v.resolved THEN s.updated_at ELSE NULL END,")
    w("  s.updated_at - interval '6 hours', s.updated_at")
    w("FROM stories s")
    w("JOIN story_versions sv ON sv.story_id = s.id AND sv.number = 1")
    w("JOIN LATERAL (VALUES")
    w("    ('This needs a second source before it runs.', 'para:3', false),")
    w("    ('Tightened the opening; check I have not changed your meaning.', 'para:1', true)")
    w("  ) AS v(body, locator, resolved) ON true")
    w("WHERE s.publication_state <> 'published';")
    w("")

    # ── media ──────────────────────────────────────────────────────────────
    w("-- Asset library: a folder tree with rights and credit recorded, because an")
    w("-- asset without those is not publishable and the library exists to prove it.")
    w("INSERT INTO drive_folders (id, name, parent_id, created_by, created_at)")
    w("SELECT gen_random_uuid(), v.name, NULL, a.id, now() - interval '60 days'")
    w("FROM (VALUES ('Newsroom photography'), ('Documents and filings'), ('Charts and graphics'), ('Audio and video')) AS v(name)")
    w("CROSS JOIN (SELECT id FROM users WHERE handle='admin') a;")
    w("")
    w("INSERT INTO assets (id, story_id, kind, uri, filename, creator, source, rights, credit, caption, alt_text, verification_status, created_by_id, folder_id, created_at, updated_at)")
    w("SELECT gen_random_uuid(), NULL, v.kind, v.uri, v.filename, v.creator, v.source, v.rights, v.credit,")
    w("  v.caption, v.alt_text, 'verified', a.id, f.id, now() - make_interval(days => v.age_d), now() - make_interval(days => v.age_d)")
    w("FROM (VALUES")
    assets = [
        ("image", "Newsroom photography", "grid-substation-evening.jpg", "A. Reddy", "Staff photograph", "Owned — staff work for hire", "iGEN News",
         "Evening load at a state transmission substation.", "Transmission towers and switchgear at a substation at dusk.", 12),
        ("image", "Newsroom photography", "fab-cleanroom-gowning.jpg", "R. Verma", "Staff photograph", "Owned — staff work for hire", "iGEN News",
         "Gowning area outside a semiconductor cleanroom.", "Workers in cleanroom suits at a gowning station.", 26),
        ("image", "Newsroom photography", "mandi-loading-dawn.jpg", "M. Patel", "Staff photograph", "Owned — staff work for hire", "iGEN News",
         "Loading before the day's first auction at a mandi.", "Sacks being loaded onto a truck before dawn.", 8),
        ("document", "Documents and filings", "tariff-order-draft.pdf", "Regulatory filing", "Public filing", "Public record", "State regulator",
         "Draft tariff order as circulated for comment.", "First page of a draft tariff order.", 5),
        ("graphic", "Charts and graphics", "capacity-additions-quarterly.svg", "Charts desk", "Staff graphic", "Owned — staff work for hire", "iGEN News",
         "Quarterly capacity additions, last eight quarters.", "Bar chart of quarterly capacity additions.", 3),
        ("audio", "Audio and video", "plant-manager-interview.m4a", "Staff recording", "Interview recording", "Owned — consent recorded", "iGEN News",
         "Interview recorded with consent at the plant.", "Audio recording of an on-site interview.", 19),
    ]
    w(",\n".join(
        f"  ('{k}', '{folder}', 'asset://library/{fn}', '{fn}', '{q(cr)}', '{q(srcv)}', '{q(rights)}', '{q(credit)}', '{q(cap)}', '{q(alt)}', {age})"
        for (k, folder, fn, cr, srcv, rights, credit, cap, alt, age) in assets))
    w(") AS v(kind, folder, uri, filename, creator, source, rights, credit, caption, alt_text, age_d)")
    w("JOIN drive_folders f ON f.name = v.folder")
    w("CROSS JOIN (SELECT id FROM users WHERE handle='admin') a;")
    w("")

    # ── desk configuration ─────────────────────────────────────────────────
    w("-- Desk operating hours and the response targets the SLA panel reports on.")
    w("INSERT INTO desk_schedules (desk_id, timezone, hours, on_call_user_id, notes, updated_at)")
    w("SELECT d.id, 'Asia/Kolkata',")
    w("  '{\"mon\":[\"09:00\",\"19:00\"],\"tue\":[\"09:00\",\"19:00\"],\"wed\":[\"09:00\",\"19:00\"],\"thu\":[\"09:00\",\"19:00\"],\"fri\":[\"09:00\",\"19:00\"],\"sat\":[\"10:00\",\"15:00\"]}'::jsonb,")
    w("  d.lead_user_id, 'Weekend cover is duty-editor only.', now() - interval '20 days'")
    w("FROM desks d WHERE d.lead_user_id IS NOT NULL;")
    w("")
    w("-- Targets are in hours and must be positive; warn_at_percent is 1-100.")
    w("INSERT INTO desk_slas (id, desk_id, workflow_state, target_hours, warn_at_percent, is_active, updated_at)")
    w("SELECT gen_random_uuid(), d.id, v.state, v.hours, 80, true, now() - interval '20 days'")
    w("FROM desks d CROSS JOIN (VALUES")
    w("    ('desk_review', 24.0), ('verification', 48.0), ('copy_standards', 12.0), ('ready', 6.0)")
    w("  ) AS v(state, hours);")
    w("")

    # ── governance ─────────────────────────────────────────────────────────
    w("-- Feature flags. Keys match ^[a-z][a-z0-9_]*$, which the API enforces.")
    w("INSERT INTO feature_flags (key, description, enabled, rollout_percent, emergency_disabled, created_at, updated_at)")
    w("VALUES")
    w("  ('rich_text_editor', 'Rich-text story surface instead of the block editor.', true, 100, false, now() - interval '90 days', now() - interval '30 days'),")
    w("  ('scheduled_releases', 'Allow scheduling a release rather than publishing now.', true, 100, false, now() - interval '90 days', now() - interval '45 days'),")
    w("  ('speculative_summaries', 'Automatic story summaries on the desk view.', false, 0, false, now() - interval '20 days', now() - interval '20 days'),")
    w("  ('front_page_autocurate', 'Suggest front-page slots from performance.', false, 25, false, now() - interval '15 days', now() - interval '5 days');")
    w("")

    w("-- Legal documents with signatories, so the legal surface is not empty.")
    w("INSERT INTO legal_documents (id, title, body_md, content_sha256, status, created_by, created_at, updated_at)")
    w("SELECT gen_random_uuid(), v.title, v.body, encode(sha256(convert_to(v.body, 'UTF8')), 'hex'), v.status, n.id,")
    w("  now() - make_interval(days => v.age_d), now() - make_interval(days => (v.age_d/2))")
    w("FROM (VALUES")
    w("  ('Freelance contributor agreement',")
    w("   '# Freelance contributor agreement' || chr(10) || chr(10) ||")
    w("   'This agreement covers commissioned work, kill fees, rights and expenses.' || chr(10) || chr(10) ||")
    w("   '## Rights' || chr(10) || chr(10) ||")
    w("   'First publication rights for ninety days, reverting to the contributor thereafter.',")
    w("   'executed', 40),")
    w("  ('Source protection policy',")
    w("   '# Source protection policy' || chr(10) || chr(10) ||")
    w("   'How the newsroom records, stores and limits access to source identities.',")
    w("   'executed', 120),")
    w("  ('Photography licence - regional agency',")
    w("   '# Photography licence' || chr(10) || chr(10) ||")
    w("   'Terms for agency images, including embargo and territory limits.',")
    w("   'pending', 6)")
    w(") AS v(title, body, status, age_d)")
    w("CROSS JOIN (SELECT id FROM users WHERE handle='n.desai') n;")
    w("")
    w("INSERT INTO legal_document_parties (id, document_id, user_id, party_role, party_kind, sign_order, invited_at, signed_at, signed_name)")
    w("SELECT gen_random_uuid(), ld.id, u.id, v.role, 'signatory', v.ord,")
    w("  ld.created_at, CASE WHEN ld.status='executed' THEN ld.created_at + interval '2 days' ELSE NULL END,")
    w("  CASE WHEN ld.status='executed' THEN u.display_name ELSE NULL END")
    w("FROM legal_documents ld")
    w("JOIN LATERAL (VALUES ('Publisher', 'admin', 1), ('Standards', 'n.desai', 2)) AS v(role, handle, ord) ON true")
    w("JOIN users u ON u.handle = v.handle;")
    w("")

    w("-- Sector access requests awaiting a desk decision.")
    w("INSERT INTO sector_applications (id, desk_id, user_id, role, status, message, created_at, updated_at)")
    w("SELECT gen_random_uuid(), d.id, u.id, v.role, v.status, v.message,")
    w("  now() - make_interval(days => v.age_d), now() - make_interval(days => v.age_d)")
    w("FROM (VALUES")
    w("  ('space', 't.george', 'producer', 'pending', 'Producing a launch-day package; need desk access.', 4),")
    w("  ('semiconductors', 'k.menon', 'contributor', 'pending', 'Working on the fab series with the tech desk.', 9),")
    w("  ('tourism', 'r.startup', 'reporter', 'approved', 'Covering the startup angle on travel platforms.', 30)")
    w(") AS v(sector_slug, handle, role, status, message, age_d)")
    w("JOIN desks d ON d.slug=v.sector_slug JOIN users u ON u.handle=v.handle;")
    w("")

    w("INSERT INTO webhook_endpoints (url, secret, description, event_types, active, created_by_id, created_at, updated_at)")
    # A deterministic placeholder, not a real secret: `gen_random_bytes` needs
    # pgcrypto, and a seed must not mint a credential anyone might trust. The
    # value is self-describing so it is obvious it has to be rotated.
    w("SELECT 'https://hooks.internal.example/newsroom',")
    w("  encode(sha256(convert_to('PLACEHOLDER-ROTATE-BEFORE-USE', 'UTF8')), 'hex'),")
    w("  'Internal notifier for publish and correction events.',")
    w("  ARRAY['story.published','correction.filed']::text[], true, a.id, now() - interval '75 days', now() - interval '75 days'")
    w("FROM (SELECT id FROM users WHERE handle='admin') a;")
    w("")

    # ── personal state behind visible controls ─────────────────────────────
    w("-- Personal state: every one of these backs a control that otherwise renders")
    w("-- an empty shelf on a freshly seeded system.")
    w("INSERT INTO follows (user_id, entity_type, entity_id, created_at)")
    w("SELECT u.id, 'desk', d.id, now() - interval '30 days'")
    w("FROM users u JOIN desk_memberships dm ON dm.user_id = u.id JOIN desks d ON d.id = dm.desk_id")
    w("ON CONFLICT DO NOTHING;")
    w("")
    w("INSERT INTO favorites (user_id, entity_type, entity_id, label, position, created_at)")
    w("SELECT s.author_id, 'story', s.id, left(s.title, 200), row_number() OVER (PARTITION BY s.author_id ORDER BY s.published_at DESC), now() - interval '5 days'")
    w("FROM stories s WHERE s.publication_state='published'")
    w("  AND (('x' || substr(md5(s.slug), 1, 8))::bit(32)::int % 7) = 0")
    w("ON CONFLICT DO NOTHING;")
    w("")
    w("INSERT INTO recent_items (user_id, entity_type, entity_id, title, visited_at)")
    w("SELECT s.author_id, 'story', s.id, left(s.title, 300), s.updated_at")
    w("FROM stories s WHERE (('x' || substr(md5(s.slug), 1, 8))::bit(32)::int % 5) = 0")
    w("ON CONFLICT DO NOTHING;")
    w("")
    w("INSERT INTO saved_searches (id, user_id, name, query, filters_json, is_shared, created_at, updated_at)")
    w("SELECT gen_random_uuid(), u.id, v.name, v.query, v.filters::jsonb, v.shared, now() - interval '25 days', now() - interval '25 days'")
    w("FROM (VALUES")
    w("  ('e.biz', 'Open corrections', 'corrections', '{\"status\":\"open\"}', true),")
    w("  ('e.tech', 'Awaiting fact-check', 'verification', '{\"workflow_state\":\"verification\"}', true),")
    w("  ('v.iyer', 'Past their slot', 'releases', '{\"status\":\"scheduled\"}', false)")
    w(") AS v(handle, name, query, filters, shared)")
    w("JOIN users u ON u.handle=v.handle;")
    w("")
    w("-- Digest and priority are closed sets: immediate|hourly|daily|off and")
    w("-- low|normal|high|critical.")
    w("INSERT INTO notification_preferences (user_id, kind, in_app, digest, min_priority, updated_at)")
    w("SELECT u.id, v.kind, true, v.digest, v.min_priority, now() - interval '40 days'")
    w("FROM users u CROSS JOIN (VALUES")
    w("    ('review_requested', 'immediate', 'normal'),")
    w("    ('correction_filed', 'immediate', 'high'),")
    w("    ('release_failed', 'immediate', 'critical'),")
    w("    ('mention', 'hourly', 'low')")
    w("  ) AS v(kind, digest, min_priority)")
    w("ON CONFLICT DO NOTHING;")
    w("")
    w("INSERT INTO workspace_branding (id, desk_id, brand_name, accent_color, ink_color, font_preset, updated_by, updated_at)")
    w("SELECT gen_random_uuid(), NULL, 'iGEN News', '#0a0a0a', '#0a0a0a', 'editorial', a.id, now() - interval '80 days'")
    w("FROM (SELECT id FROM users WHERE handle='admin') a;")
    w("")





def build_governance_extras(w, plan, published, inflight) -> None:
    """Governance, ops and audience tables — the last block of empty tables.

    Everything here is derived from rows the seed has already written rather than
    invented alongside them. That matters most for `attention_states`: the service
    refuses to record acknowledgement or a snooze for a fingerprint its detectors
    do not currently produce, so a state row keyed on anything else would be data
    the product itself rejects. The fingerprints below are built by joining the
    real reviews, tasks, corrections and releases, exactly as `DETECTORS` does.
    """
    w("-- ---------------------------------------------------------------------")
    w("-- Governance, operations and audience")
    w("-- ---------------------------------------------------------------------")
    w("")

    w("-- A wider task board. Six tasks across fifty desks left the board, the")
    w("-- workload panels and the overdue half of the attention queue effectively")
    w("-- empty. Due dates are relative so a fraction is always genuinely overdue.")
    w("INSERT INTO tasks (id, story_id, desk_id, title, description, status, priority,")
    w("                   assigned_to_id, created_by_id, due_at, completed_at, blocker, created_at, updated_at)")
    w("SELECT gen_random_uuid(), s.id, s.desk_id, v.title, v.description, v.status, v.priority,")
    w("       s.author_id, s.author_id,")
    w("       s.created_at + (v.due_offset_d || ' days')::interval,")
    w("       CASE WHEN v.status = 'done' THEN s.created_at + interval '3 days' ELSE NULL END,")
    w("       CASE WHEN v.status = 'blocked' THEN 'Waiting on a response from the ministry press office' ELSE NULL END,")
    w("       s.created_at, s.created_at + interval '1 day'")
    w("FROM (")
    w("  SELECT id, desk_id, author_id, created_at,")
    w("         row_number() OVER (ORDER BY created_at DESC) AS rn")
    w("  FROM stories")
    w(") s")
    w("JOIN (VALUES")
    w("    (0, 'Verify the primary figure with the source', 'Confirm the headline number against the released document before filing.', 'done', 'high', 2),")
    w("    (1, 'Second read for desk', 'Desk editor pass for structure and framing.', 'in_progress', 'medium', 4),")
    w("    (2, 'Chase the ministry for comment', 'Right of reply has been requested; log the response or the refusal.', 'blocked', 'high', -3),")
    w("    (3, 'Commission a chart for the data section', 'Graphics desk to turn the table into a chart sized for mobile.', 'todo', 'low', 6),")
    w("    (4, 'Fact-check the three named claims', 'Each claim needs a primary document attached before publication.', 'in_review', 'urgent', -1),")
    w("    (5, 'Confirm image rights and credit line', 'Licence covers web and social only; check territory before use.', 'todo', 'medium', 5)")
    w("  ) AS v(slot, title, description, status, priority, due_offset_d)")
    w("  ON (s.rn % 9) = v.slot")
    w("WHERE s.rn <= 90;")
    w("")

    # A failed release is the only way the publishing queue's error columns, and
    # the `release_failed` attention detector, ever show anything. It must belong
    # to a story that is *not* live: a release that failed did not publish, and
    # the "every published story has a published release" invariant would other-
    # wise be contradicted by its own fixture.
    w("-- Two failed deliveries, on stories that are deliberately NOT published —")
    w("-- a release that failed is precisely one that did not go live. Without")
    w("-- these the publishing queue's error columns and the release_failed")
    w("-- detector have nothing to render.")
    w("INSERT INTO releases (id, story_id, version_id, release_type, status, channels, receipts,")
    w("                      scheduled_at, published_at, created_by_id, created_at)")
    w("SELECT gen_random_uuid(), s.id, sv.id, 'initial', v.status, '[\"web\",\"newsletter\"]'::jsonb, '{}'::jsonb,")
    w("       now() - (v.age_h || ' hours')::interval, NULL, s.author_id, now() - (v.age_h || ' hours')::interval")
    w("FROM (")
    w("  SELECT id, author_id, row_number() OVER (ORDER BY created_at DESC) AS rn")
    w("  FROM stories WHERE publication_state <> 'published'")
    w(") s")
    w("JOIN story_versions sv ON sv.story_id = s.id")
    w("JOIN (VALUES (1, 'failed', 5), (2, 'partial_failure', 20)) AS v(rn, status, age_h) ON v.rn = s.rn;")
    w("")
    w("-- Attempt trail for those two: a retry that also failed, so attempt_count,")
    w("-- last_error_code and last_error_message are all exercised.")
    w("INSERT INTO release_attempts (id, release_id, attempt_number, status, trigger,")
    w("                              started_at, completed_at, error_code, error_message)")
    w("SELECT gen_random_uuid(), r.id, v.n, v.status, 'worker',")
    w("       r.created_at + ((v.n - 1) * 12 || ' minutes')::interval,")
    w("       r.created_at + ((v.n - 1) * 12 + 1 || ' minutes')::interval,")
    w("       v.code, v.message")
    w("FROM releases r")
    w("JOIN (VALUES")
    w("    (1, 'failed', 'newsletter_timeout', 'Newsletter channel did not acknowledge within 30s'),")
    w("    (2, 'failed', 'newsletter_timeout', 'Retry failed the same way; channel is still not acknowledging')")
    w("  ) AS v(n, status, code, message) ON true")
    w("WHERE r.status IN ('failed', 'partial_failure');")
    w("")

    w("-- Attention state. Fingerprints are built the same way DETECTORS builds")
    w("-- them ('review:'||id and so on), because attention.rs refuses to record")
    w("-- state for a fingerprint no detector currently produces — a hand-written")
    w("-- fingerprint would be a row the application itself would reject.")
    w("INSERT INTO attention_states (fingerprint, user_id, entity_type, entity_id, attention_type,")
    w("                              first_detected_at, last_detected_at, acknowledged_at, snoozed_until, metadata_json)")
    w("SELECT 'review:' || r.id, r.assigned_to_id, 'review', r.id::text,")
    w("       CASE WHEN r.created_at <= now() - interval '48 hours' THEN 'review_overdue' ELSE 'review_pending' END,")
    w("       r.created_at, now() - interval '2 hours',")
    w("       CASE WHEN x.rn % 3 = 0 THEN now() - interval '6 hours' ELSE NULL END,")
    w("       CASE WHEN x.rn % 5 = 0 THEN now() + interval '2 days' ELSE NULL END,")
    w("       '{}'::jsonb")
    w("FROM reviews r")
    w("JOIN (SELECT id, row_number() OVER (ORDER BY created_at) AS rn FROM reviews WHERE decision='pending') x")
    w("  ON x.id = r.id")
    w("WHERE r.decision = 'pending' AND r.assigned_to_id IS NOT NULL")
    w("ON CONFLICT DO NOTHING;")
    w("")
    w("INSERT INTO attention_states (fingerprint, user_id, entity_type, entity_id, attention_type,")
    w("                              first_detected_at, last_detected_at, acknowledged_at, escalated_at, metadata_json)")
    w("SELECT 'correction:' || c.id, c.owner_id, 'correction', c.id::text, 'correction_open',")
    w("       c.created_at, now() - interval '1 hour', now() - interval '20 hours',")
    w("       now() - interval '18 hours', '{}'::jsonb")
    w("FROM corrections c WHERE c.status = 'open' AND c.owner_id IS NOT NULL")
    w("ON CONFLICT DO NOTHING;")
    w("")
    w("INSERT INTO attention_states (fingerprint, user_id, entity_type, entity_id, attention_type,")
    w("                              first_detected_at, last_detected_at, snoozed_until, metadata_json)")
    w("SELECT 'task:' || t.id, t.assigned_to_id, 'task', t.id::text, 'task_overdue',")
    w("       t.due_at, now() - interval '30 minutes', now() + interval '1 day', '{}'::jsonb")
    w("FROM (SELECT id, assigned_to_id, due_at, row_number() OVER (ORDER BY due_at) AS rn")
    w("      FROM tasks WHERE due_at < now() AND status NOT IN ('done','cancelled')")
    w("        AND assigned_to_id IS NOT NULL) t")
    w("WHERE t.rn % 4 = 1")
    w("ON CONFLICT DO NOTHING;")
    w("")

    w("-- Saved dashboard views: one personal default each for three editors, plus")
    w("-- one shared desk view so the 'shared with the desk' branch is exercised.")
    w("INSERT INTO dashboard_views (id, user_id, desk_id, name, description, is_default, is_shared,")
    w("                             layout_json, filters_json, created_at, updated_at)")
    w("SELECT gen_random_uuid(), u.id, d.id, v.name, v.description, v.is_default, v.is_shared,")
    w("       v.layout::jsonb, v.filters::jsonb, now() - interval '60 days', now() - interval '9 days'")
    w("FROM (VALUES")
    w(f"    ('e.biz', '{FIXTURE_DESKS['finance']}', 'Morning desk check', 'Queue, overdue reviews and today''s releases.',")
    w("      true, false, '[\"attention\",\"publishing_queue\",\"workload\"]', '{\"window\":\"today\"}'),")
    w(f"    ('e.tech', '{FIXTURE_DESKS['tech']}', 'Week ahead', 'Scheduled releases and open corrections for the week.',")
    w("      true, false, '[\"publishing_queue\",\"corrections\",\"calendar\"]', '{\"window\":\"7d\"}'),")
    w("    ('n.desai', NULL, 'Standards review', 'Corrections ledger and source approvals, newsroom wide.',")
    w("      true, false, '[\"corrections\",\"sources\",\"attention\"]', '{\"scope\":\"global\"}'),")
    w(f"    ('e.health', '{FIXTURE_DESKS['health']}', 'Health desk — shared', 'The view the whole desk opens on.',")
    w("      false, true, '[\"attention\",\"workload\",\"coverage\"]', '{\"window\":\"14d\"}')")
    w("  ) AS v(handle, desk_slug, name, description, is_default, is_shared, layout, filters)")
    w("JOIN users u ON u.handle = v.handle")
    w("LEFT JOIN desks d ON d.slug = v.desk_slug;")
    w("")

    # Delegation and permission grants are the two places a seed can quietly widen
    # access. Both are therefore time-boxed, and one of each is inert (revoked /
    # denied) so the surface is demonstrated without leaving standing privilege.
    w("-- Delegations are time-boxed on purpose. A seed that leaves a standing")
    w("-- grant of reviews.decide_any behind is a seed that silently widens access")
    w("-- on every database it touches; the active one expires, and the second is")
    w("-- already revoked so the revoked branch renders too.")
    w("INSERT INTO delegations (id, from_user_id, to_user_id, capabilities, starts_at, ends_at, reason, revoked_at, created_at)")
    w("SELECT gen_random_uuid(), f.id, t.id, v.caps::jsonb,")
    w("       now() - (v.start_d || ' days')::interval, now() + (v.end_d || ' days')::interval,")
    w("       v.reason,")
    w("       CASE WHEN v.revoked THEN now() - interval '4 days' ELSE NULL END,")
    w("       now() - (v.start_d || ' days')::interval")
    w("FROM (VALUES")
    w("    ('e.biz', 'r.fintech', '[\"reviews.decide_any\",\"workflow.advance\"]', 3, 4,")
    w("      'Desk editor on leave 3-7 Sep; cover for sign-off only.', false),")
    w("    ('e.tech', 'r.chip', '[\"reviews.decide_any\"]', 20, 10,")
    w("      'Conference cover. Revoked early on return.', true)")
    w("  ) AS v(from_handle, to_handle, caps, start_d, end_d, reason, revoked)")
    w("JOIN users f ON f.handle = v.from_handle")
    w("JOIN users t ON t.handle = v.to_handle;")
    w("")

    w("-- Permission overrides: two narrow, expiring allows and one explicit deny.")
    w("-- The deny matters — it is the only row that proves the surface can take")
    w("-- privilege away rather than only hand it out.")
    w("INSERT INTO permission_policies (id, subject_type, subject_id, capability, allow, expires_at,")
    w("                                 reason, granted_by_id, version, created_at, updated_at)")
    w("SELECT gen_random_uuid(), 'user', u.id, v.capability, v.allow,")
    w("       CASE WHEN v.expires_d IS NULL THEN NULL ELSE now() + (v.expires_d || ' days')::interval END,")
    w("       v.reason, a.id, 1, now() - interval '30 days', now() - interval '30 days'")
    w("FROM (VALUES")
    w("    ('r.pillai', 'frontpage.curate', true, 14, 'Covering front-page duty while the news editor is away.'),")
    w("    ('t.george', 'dashboard.view_global', true, 45, 'Audience reporting across all desks for the quarterly review.'),")
    w("    ('r.startup', 'stories.delete_any', false, NULL, 'Explicitly withheld: deletion stays with the standards desk.')")
    w("  ) AS v(handle, capability, allow, expires_d, reason)")
    w("JOIN users u ON u.handle = v.handle")
    w("CROSS JOIN (SELECT id FROM users WHERE handle = 'admin') a")
    w("ON CONFLICT DO NOTHING;")
    w("")

    w("INSERT INTO desk_invitations (id, desk_id, user_id, invited_by_id, desk_role, status, message, responded_at, created_at)")
    w("SELECT gen_random_uuid(), d.id, u.id, i.id, v.desk_role, v.status, v.message,")
    w("       CASE WHEN v.status = 'pending' THEN NULL ELSE now() - (v.age_d - 1 || ' days')::interval END,")
    w("       now() - (v.age_d || ' days')::interval")
    w("FROM (VALUES")
    w(f"    ('r.renew', '{FIXTURE_DESKS['climate']}', 'member', 'pending',")
    w("      'You have filed twice on the desk this month — join it properly?', 4),")
    w(f"    ('r.space', '{FIXTURE_DESKS['defence']}', 'member', 'accepted', 'Cross-desk cover for launch coverage.', 30),")
    w(f"    ('r.tourism', '{FIXTURE_DESKS['logistics']}', 'member', 'declined',")
    w("      'Would you take the aviation beat?', 22),")
    w(f"    ('r.ai', '{FIXTURE_DESKS['aicyber']}', 'editor', 'pending',")
    w("      'Standing in as desk editor from next quarter.', 2)")
    w("  ) AS v(handle, desk_slug, desk_role, status, message, age_d)")
    w("JOIN users u ON u.handle = v.handle")
    w("JOIN desks d ON d.slug = v.desk_slug")
    w("JOIN (SELECT id FROM users WHERE handle = 'admin') i ON true;")
    w("")

    # `desk_smtp_settings` holds a password column. A seed must not mint anything
    # that could be mistaken for a working credential, and must not leave a desk
    # configured to attempt delivery through a host that does not exist.
    w("-- SMTP config is seeded INACTIVE with a self-describing placeholder. The")
    w("-- row exists so the settings screen is not blank, but nothing will attempt")
    w("-- delivery through a host that does not resolve, and there is no value here")
    w("-- that could be mistaken for a working credential.")
    w("INSERT INTO desk_smtp_settings (desk_id, host, port, username, password, from_address,")
    w("                                from_name, use_starttls, active, updated_by_id, updated_at)")
    w("SELECT d.id, 'smtp.invalid', 587, 'PLACEHOLDER', 'PLACEHOLDER-SET-BEFORE-ENABLING',")
    w("       v.from_address, v.from_name, true, false, a.id, now() - interval '70 days'")
    w("FROM (VALUES")
    w(f"    ('{FIXTURE_DESKS['finance']}', 'business@news.invalid', 'iGEN News — Business'),")
    w(f"    ('{FIXTURE_DESKS['tech']}', 'technology@news.invalid', 'iGEN News — Technology')")
    w("  ) AS v(desk_slug, from_address, from_name)")
    w("JOIN desks d ON d.slug = v.desk_slug")
    w("CROSS JOIN (SELECT id FROM users WHERE handle = 'admin') a;")
    w("")

    w("INSERT INTO feed_reposts (post_id, user_id, created_at)")
    w("SELECT p.id, u.id, p.created_at + interval '3 hours'")
    w("FROM (SELECT id, created_at, row_number() OVER (ORDER BY created_at DESC) AS rn FROM feed_posts) p")
    w("JOIN (SELECT id, row_number() OVER (ORDER BY handle) AS rn FROM users WHERE handle <> 'admin') u")
    w("  ON (u.rn % 7) = (p.rn % 7)")
    w("WHERE p.rn <= 6 AND u.rn <= 21")
    w("ON CONFLICT DO NOTHING;")
    w("")

    w("-- Source access grants: who may see an identity behind a protected source.")
    w("-- Only the sources whose ground rule actually needs protection.")
    w("INSERT INTO source_access_grants (source_id, user_id, granted_by_id, granted_at)")
    w("SELECT s.id, e.id, n.id, s.created_at + interval '1 day'")
    w("FROM sources s")
    w("JOIN stories st ON st.id = s.story_id")
    w("JOIN desk_memberships dm ON dm.desk_id = st.desk_id AND dm.role = 'section_editor'")
    w("JOIN users e ON e.id = dm.user_id")
    w("CROSS JOIN (SELECT id FROM users WHERE handle = 'n.desai') n")
    w("WHERE s.ground_rule IN ('deep_background', 'off_record')")
    w("ON CONFLICT DO NOTHING;")
    w("")

    w("-- Metric snapshots are COMPUTED from the seeded releases rather than")
    w("-- invented next to them, so the dashboard trend and the publishing history")
    w("-- cannot disagree with each other.")
    w("INSERT INTO metric_snapshots (id, metric_key, scope_type, scope_id, bucket_start, bucket_end, value, created_at)")
    w("SELECT gen_random_uuid(), 'stories_published', 'global', '',")
    w("       date_trunc('day', r.published_at), date_trunc('day', r.published_at) + interval '1 day',")
    w("       count(*), now()")
    w("FROM releases r WHERE r.status = 'published' AND r.published_at IS NOT NULL")
    w("GROUP BY date_trunc('day', r.published_at)")
    w("ON CONFLICT DO NOTHING;")
    w("")
    w("INSERT INTO metric_snapshots (id, metric_key, scope_type, scope_id, bucket_start, bucket_end, value, created_at)")
    w("SELECT gen_random_uuid(), 'stories_published', 'desk', s.desk_id::text,")
    w("       date_trunc('week', r.published_at), date_trunc('week', r.published_at) + interval '7 days',")
    w("       count(*), now()")
    w("FROM releases r JOIN stories s ON s.id = r.story_id")
    w("WHERE r.status = 'published' AND r.published_at IS NOT NULL AND s.desk_id IS NOT NULL")
    w("GROUP BY s.desk_id, date_trunc('week', r.published_at)")
    w("ON CONFLICT DO NOTHING;")
    w("")

    # Reserved-for-documentation domains (RFC 2606) only: a seed must not be able
    # to send mail to, or look like it holds, a real reader's address.
    w("-- Audience. Addresses use RFC 2606 reserved domains only — a seed must not")
    w("-- carry anything that could route to a real person's inbox.")
    w("INSERT INTO subscriptions (id, subscriber_email, subscriber_name, plan, status, source,")
    w("                           external_ref, started_at, current_period_end, canceled_at,")
    w("                           created_by_id, created_at, updated_at)")
    w("SELECT gen_random_uuid(), v.email, v.name, v.plan, v.status, v.source, v.ref,")
    w("       now() - (v.age_d || ' days')::interval,")
    w("       CASE WHEN v.status = 'canceled' THEN NULL ELSE now() + interval '30 days' END,")
    w("       CASE WHEN v.status = 'canceled' THEN now() - interval '11 days' ELSE NULL END,")
    w("       a.id, now() - (v.age_d || ' days')::interval, now() - interval '11 days'")
    w("FROM (VALUES")
    w("    ('a.krishnan@example.com', 'Anita Krishnan', 'annual', 'active', 'web', 'sub_seed_0001', 220),")
    w("    ('d.mehta@example.com', 'Devang Mehta', 'standard', 'active', 'web', 'sub_seed_0002', 190),")
    w("    ('library@example.org', 'Regional Library Service', 'institutional', 'active', 'manual', 'sub_seed_0003', 400),")
    w("    ('p.nair@example.net', 'Priya Nair', 'standard', 'trialing', 'web', 'sub_seed_0004', 9),")
    w("    ('s.iyengar@example.com', 'Suresh Iyengar', 'annual', 'past_due', 'web', 'sub_seed_0005', 370),")
    w("    ('m.fernandes@example.com', 'Maria Fernandes', 'standard', 'canceled', 'web', 'sub_seed_0006', 260),")
    w("    ('policy.desk@example.org', 'Policy Research Unit', 'institutional', 'active', 'manual', 'sub_seed_0007', 150),")
    w("    ('k.reddy@example.com', 'Kavya Reddy', 'standard', 'active', 'web', 'sub_seed_0008', 45)")
    w("  ) AS v(email, name, plan, status, source, ref, age_d)")
    w("CROSS JOIN (SELECT id FROM users WHERE handle = 'admin') a")
    w("ON CONFLICT DO NOTHING;")
    w("")


if __name__ == "__main__":
    main()
