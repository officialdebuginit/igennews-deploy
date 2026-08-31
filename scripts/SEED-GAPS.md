# Seed coverage — what was missing, and what closed it

Measured against a live database seeded from `seed-sectors.sql` + `seed-igennews.sql`
(60 tables in the `meridian` schema).

**Current state: 50 of 60 tables populated.** The ten still empty are runtime-only
and listed at the foot of this document. Skip to
[Closed by the rebuild](#closed-by-the-rebuild) for what the seed produces today.

Everything from here to that section is the **original gap analysis** — the state
before the rebuild — kept because it records what was wrong and why it mattered.

## Summary — before the rebuild

| | Tables | Note |
|---|---|---|
| Seeded by these scripts | **11** | content and org structure |
| Populated at runtime, correctly not seeded | **9** | sessions, audit, notifications, webhooks |
| **Empty — no seed, no runtime data** | **40** | the gap |

The seed creates *content* but almost none of the *editorial machinery* that makes
the content coherent. A story is marked published without ever having been
versioned, reviewed, approved or released.

## What is seeded

| Table | Rows |
|---|---|
| `sub_sectors` | 1348 |
| `stories` | 90 |
| `desks` | 50 |
| `desk_memberships` | 44 |
| `users` | 35 |
| `role_assignments` | 24 |
| `tasks` | 16 |
| `feed_posts` | 14 |
| `pitches` | 8 |
| `coverage_events` | 8 |
| `front_page_slots` | 6 |

## Gap 1 — the editorial trail is absent

The most consequential gap. Every published story asserts an outcome with no
record of how it got there.

| Table | Why it matters |
|---|---|
| `story_versions` | 58 stories are `publication_state='published'` with **zero versions**. Reviews attach to a version, so nothing can be reviewed. |
| `reviews` | No copy, desk or fact-check review exists anywhere. |
| `approvals` | The three required sign-offs are never recorded. |
| `workflow_state_history` | No story has a state history, so the audit trail the product promises is empty. |
| `releases` | **58 published stories, 0 release rows.** Publication is asserted on the story row alone; the publishing queue is therefore always empty. |
| `release_attempts` | No delivery record, so the queue's attempt/error columns are always null. |
| `corrections` | The corrections ledger — a trust surface linked from every article — is empty. |

## Gap 2 — verification chain absent

| Table | Why it matters |
|---|---|
| `sources` | No sourcing on any story. |
| `evidence` | Nothing supporting any claim. |
| `claims` | The claim-verification workflow has no data. |
| `source_access_grants` | Source protection is unexercised. |

## Gap 3 — no media

`assets` and `drive_folders` are empty. No story has a lead image, and the asset
library — a top-level navigation item — opens empty. Feed photo posts cannot be
demonstrated.

## Gap 4 — engagement and personal surfaces

`comments`, `feed_likes`, `feed_replies`, `feed_reposts`, `follows`, `favorites`,
`recent_items`, `saved_searches`, `dashboard_views`, `notification_preferences`,
`subscriptions`, `attention_states`.

Every one of these backs a visible control. They are all empty, so those controls
render zero-states on a freshly seeded system.

## Gap 5 — governance and configuration

`feature_flags`, `permission_policies`, `delegations`, `desk_invitations`,
`desk_schedules`, `desk_slas`, `desk_smtp_settings`, `webhook_endpoints`,
`webhook_deliveries`, `sector_applications`, `legal_documents`,
`legal_document_parties`, `workspace_branding`, `metric_snapshots`.

## Gap 6 — sector coverage is 36%

**Only 18 of 50 sectors have any stories. 32 sectors are empty.** A seeded system
presents fifty sectors in navigation, and two thirds of them lead to nothing.

## Gap 7 — articles are too short to be representative

| | Words |
|---|---|
| Shortest | 137 |
| Median | 163 |
| Longest | 178 |

Roughly nine blocks each. Real reported pieces run 600–1500 words with internal
structure. At this length the reader page cannot demonstrate subheads, pull
quotes, long-form pacing or reading-time estimates that mean anything — every
article reports "1 min read".

## Closed by the rebuild

`build_seed_content.py` regenerates everything from the stories block down, from the
long-form article set in `articles/`. Verified by applying `seed-sectors.sql` +
`seed-igennews.sql` to an empty database built from the real schema.

**50 of 60 tables are now populated, up from 11.** The ten that remain empty are
the runtime-only ones listed at the foot of this document; there is no longer a
table that is empty because the seed forgot it.

### Content and the editorial trail

| | Before | After |
|---|---|---|
| Stories | 90 | **150
** |
| Sectors with stories | 18 / 50 | **50 / 50** |
| Article length (words) | 137 – 178, median 163 | **806 – 1116, median 941** |
| `story_versions` | 0 | **150
** |
| `reviews` | 0 | **345
** (315 approved, 30 pending) |
| `approvals` | 0 | **315
** |
| `workflow_state_history` | 0 | **630
** |
| `releases` | 0 | **110
** (105 published, 3 scheduled, 1 failed, 1 partial) |
| `release_attempts` | 0 | **109
** |
| `corrections` | 0 | **3
** (2 resolved, 1 open) |

### Verification chain (Gap 2)

| | After |
|---|---|
| `sources` | **175
**, of which **70** are deep-background or off-record |
| `evidence` | **280
** |
| `claims` | **70
** |
| `source_access_grants` | **22
** |

### Media, engagement and personal surfaces (Gaps 3–4)

`assets` **6
**, `drive_folders` **4
**, `comments` **90
**,
`feed_likes` **36
**, `feed_replies` **5
**, `feed_reposts` **18
**,
`follows` **42
**, `favorites` **17
**, `recent_items` **22
**,
`saved_searches` **3
**, `dashboard_views` **4
**,
`notification_preferences` **140
**, `subscriptions` **8
**,
`attention_states` **43
**, `tasks` **66
** (was 6).

### Governance and configuration (Gap 5)

`feature_flags` **4
**, `permission_policies` **3
**,
`delegations` **2
**, `desk_invitations` **4
**,
`desk_schedules` **18
**, `desk_slas` **200
**,
`desk_smtp_settings` **2
**, `webhook_endpoints` **1
**,
`sector_applications` **3
**, `legal_documents` **3
**,
`legal_document_parties` **6
**,
`workspace_branding` **1
**, `metric_snapshots` **107
**.

## What the seed refuses to fabricate

Three categories are deliberately constrained rather than filled in with whatever
would look fullest:

- **Credentials.** `desk_smtp_settings` is seeded with a self-describing
  placeholder password, host `smtp.invalid`, and `active = false`. The settings
  screen is not blank, nothing will attempt delivery, and no value in the seed can
  be mistaken for one that works. The webhook signing secret is a fixed
  `PLACEHOLDER-ROTATE-BEFORE-USE` digest for the same reason.
- **Standing privilege.** Every `delegations` row and every `allow` in
  `permission_policies` is time-boxed; one delegation is already revoked and one
  policy is an explicit `deny`. A seed that left a permanent grant of
  `reviews.decide_any` behind would widen access on every database it touched.
- **Reachable addresses.** Subscriber emails use only RFC 2606 reserved domains
  (`example.com`, `example.org`, `example.net`), so no seeded row can route mail
  to a real person.

## Integrity checks on the seeded result

- Every published story has a published release — **0 missing**.
- **0 approvals where the approver is the story's own author.** The application
  refuses a sign-off from an author on their own story, so a seed that violated
  this would produce data the product itself would reject.
- Exactly one open `workflow_state_history` row per story, as
  `workflow_state_history_open_uq` requires.
- No published story carries a still-pending review.
- **0 failed releases attached to a published story.** A release that failed is
  precisely one that did not go live; the two failures sit on in-flight stories.
- **0 `attention_states` rows without a live detector item.** `attention.rs`
  refuses to record acknowledgement or a snooze for a fingerprint its detectors do
  not currently produce, so the seed builds fingerprints by joining the real
  reviews, tasks, corrections and releases exactly as `DETECTORS` does. A
  hand-written fingerprint would be a row the application itself would reject.
- `metric_snapshots` totals are **computed from** the seeded releases (105 = 105),
  not invented alongside them, so the dashboard trend cannot disagree with the
  publishing history.

Verified by running the application's own SQL against the seeded database rather
than only checking row counts: the `DETECTORS` union returns items in all four
categories and all three severities, with acknowledged, snoozed and escalated
states represented; the `dashboard.rs` release-timeline query returns populated
`attempt_count`, `last_attempt_status`, `last_error_code` and `last_error` columns,
which were previously null for every row because no release had ever failed.

Three schema rules the previous seed never had to satisfy, and which this one now
respects: `workflow_state` has **no** `published` member (publication is a separate
axis, and the pipeline ends at `ready`); unpublished work is `not_live`, not
`draft`; and `corrections.classification` does not include `factual_error` — the
term is `correction`.

## Guards added to the generator

Three defects in this work were caught late and are now build failures rather than
things to notice afterwards:

- **`validate()` refuses content that reintroduces the duplicated headline.**
- **`FIXTURE_DESKS` is checked against `subsectors.json`.** A desk slug that does
  not exist does not raise an error — it disappears through the JOIN and leaves the
  fixture silently half-seeded. Four rows were lost that way before this check.
- **Re-runnability is asserted before the file is written.** The seed documents
  itself as "a clean reset" and the deploy runbook tells operators to re-run it;
  that only holds if every table written is also cleared by the preamble.
  `subscriptions`, `feature_flags`, `webhook_endpoints` and `desk_smtp_settings`
  were not, so the second run died on a primary-key collision. The generator now
  refuses to emit a seed with that property, and applying the seed three times in
  succession produces identical counts.

## Closed since: the seed is now self-sufficient

Two things stopped an empty database from being seeded by the documented command.

- **`role_definitions` had to be projected by the app first.** `seed-igennews.sql`
  inserts `role_assignments`, which has a foreign key to it, and neither content
  seed wrote it — so a genuinely empty database failed on the role-assignment
  insert until the app had been started once. `seed-roles.sql` now carries that
  projection, captured from the application's own output and verified identical to
  it (12 roles, 183 capability grants). It is a bootstrap, not a source of truth:
  the app re-projects on every boot with the same `ON CONFLICT` semantics, so a
  stale copy repairs itself rather than overriding the registry.
- **The documented seeding role could not run the seed.** Both runbooks and
  `ACCOUNTS.md` told operators to seed with `DATABASE_DIRECT_URL`. The content seed
  opens with `TRUNCATE`, which requires table ownership, and the application role is
  not granted it — `has_table_privilege('meridian_app', 'meridian.stories',
  'TRUNCATE')` is false while the migrator role's is true. Seeding as the app user
  failed on the first statement. `scripts/seed-all.sh` now checks connectivity,
  schema presence and TRUNCATE privilege before writing anything, and names the role
  to use; the three documents were corrected.

`scripts/seed-all.sh` runs `seed-sectors.sql` → `seed-roles.sql` →
`seed-igennews.sql` in order. Verified end to end: an empty database with no
`role_definitions` and no prior app boot reaches 150 stories across 50 sectors, 35
people and the full editorial trail in about half a second, and three consecutive
runs leave identical counts.

## Verified against a database built from the migrations

Everything above was originally checked against a database restored from a dump of
the development database. That is not the same thing as a database a deployment
would actually have, and the difference mattered: **the dev database carries 14
columns on `users` that no migration creates** — `title`, `bio`, `location`,
`pronouns`, `phone`, `timezone`, `languages`, `skills`, `availability_status`,
`profile_visibility` and four more. No application SQL reads or writes any of them;
the app selects only the migration-created columns, and the curated profile lives in
the `profile` jsonb that migration `0026_user_profile` adds.

The seed was writing ten of those drifted columns, so **it failed on the first
`INSERT` against a freshly migrated database** — `column "title" of relation "users"
does not exist`. Nothing was lost by removing them: every value was already
duplicated inside `profile`, which is what the application actually reads. All 35
seeded profiles still carry bio, location and skills.

The seed is now verified end to end on a database built the way a deployment builds
one: `deploy/postgres/roles.sql` for the schema and grants, then all 44 migrations
applied as `meridian_migrator`, then `seed-all.sh`. That path also confirms the
privilege split is real rather than a local artifact — `meridian_migrator` owns the
tables and can `TRUNCATE`; `meridian_app` can `INSERT` but not `TRUNCATE`.

`check-seed-columns.py` now runs from `seed-all.sh` before anything is written. It
compares every `INSERT` column list in all three seed files against the target
database's `information_schema` and reports **all** mismatches at once. Postgres
reports only the first, and only once execution reaches it — mid-file, with most of
the seed already applied and no indication of how many more are wrong.

## Still open

- **`sources.access_level` is written but never read.** The column has no check
  constraint and nothing in the codebase gates on it. The seed uses only the two
  values already in evidence (`story_team`, the schema default, for protected
  sources; `desk` otherwise) rather than inventing a vocabulary, but source
  protection is not actually enforced by this column today.

## Fixed already

- **Duplicated headline.** Every story body opened with a `heading` block repeating
  its own title, so each article printed its headline twice. Removed from the
  generator and from every row of `seed-igennews.sql`. The reader also guards
  against it, because databases already seeded with the old data still carry the
  duplicate.

## Correctly *not* seeded

`sessions`, `audit_events`, `notifications`, `webhook_events`, `webhook_deliveries`,
`presence`, `password_reset_tokens`, `channels`, `_sqlx_migrations`,
`rust_platform_migrations`. These are produced by running the system; seeding them
would fabricate history.
