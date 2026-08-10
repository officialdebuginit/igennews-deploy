# Meridian — Two-Level Sector Taxonomy & Sector Governance

*Design document. Prepared 2026-08-10. Documentation only — no implementation, no migration, no source change is made by this document. Every current-state claim cites `file:line` in the `meridian-rust` tree at the time of writing (branch `main`, tip `57eca7c`).*

> **Product ask (verbatim from the owner).** (1) A richer **two-level taxonomy**: top-level **Sectors** that each contain **Sub-sectors / Industries**; an article is filed under a specific industry (e.g. *Economy → Agriculture / Coal / Steel …*). (2) Seed ~40 industries (list in §2.3), de-duplicated under a small set of top-level Sectors. (3) **Governance**: only an admin — or a user explicitly granted a sector-management role/capability — may add/remove/edit sectors and sub-sectors, manage their **policies**, and handle **applications**; a single delegated user should be able to fully manage one sector, while global add/remove stays admin-only.

---

## 0. Executive summary

- **Sectors already exist — as desks.** The memory note "sectors ARE desks" is correct. There is no separate Sector entity: the sector switcher, `/s/:sector/*` routes, the applications flow, and the `Scope::Sector` authz boundary are all keyed on the `meridian.desks` table (`crates/newsroom/src/desks.rs:1-5`, `crates/newsroom/src/authz.rs:227-254`, `crates/web/src/main.rs:102-127`).
- **A story attaches to exactly one sector** via `stories.desk_id` (nullable) — the authorization boundary for every editorial action — plus a **free-text `category`** string that is unstructured today (`crates/newsroom/src/stories.rs:122,129`, seed uses `'Politics'`/`'Business'`, `scripts/seed-newsroom.sql:139`). There is **no industry/sub-sector concept anywhere** in the codebase (confirmed by a full-tree search).
- **Recommendation: add a dedicated `sub_sectors` table** parented to a desk, and give `stories` a nullable `sub_sector_id` — *not* model industries as child desks. Child desks would explode the workspace machinery (memberships, SLAs, schedules, branding, webhooks, analytics roll-ups) at industry granularity, collide on the global `desks.slug` unique index, and — because membership does not inherit down `parent_id` — break the "write under any industry" ask.
- **Governance reuses existing machinery.** Per-sector management is already expressible as a **desk-scoped `desks.manage`** grant (a `role_assignments` row with `scope_type='desk'`) or by being the desk's `lead_user_id`. The one real gap: **top-level create/delete is not admin-only today** (it defaults to the `EDITORS` audience). Close it with a new **global** capability `sectors.admin` (default `ADMIN_ONLY`) gating org-wide add/remove, while keeping desk-scoped `desks.manage` for per-sector edit / sub-sector CRUD / policy / applications.
- **Sector policy already has a home:** the desk's `settings jsonb` column, documented as "the desk's registry profile (policy, documents, metadata)" (`crates/newsroom/src/desks.rs:158-160`). This document specifies a JSON shape for it.
- **Rollout is phased and non-breaking:** `sub_sector_id` is nullable, `desk_id`/`category` are untouched, `sync_system_roles` picks up new capabilities on boot, and the webhook catalogue gains `subsector.*` and `sector.policy.updated` additively.

---

## 1. Current-state analysis (grounded)

### 1.1 Sectors are desks

The domain has one structural entity — the **desk** — and the UI/product layer calls it a *sector* / *workspace*. The module header is explicit:

> "A desk is what the UI calls a workspace; there is no separate Workspace entity. Membership deliberately does not inherit down the `parent_id` tree — that is reporting/rollup structure only." — `crates/newsroom/src/desks.rs:3-5`

Evidence that "sector" and "desk" are the same row:

- The **applications** API is literally named "Sector applications" but operates on `desk_id`: `sectors_directory()` selects from `meridian.desks` (`desks.rs:957-963`); `apply_to_sector(desk_id, …)` inserts into `sector_applications(desk_id, …)` (`desks.rs:967-1004`).
- The **authz** layer's scope parameter is named a sector but typed as a desk id: *"`desk_id` is the sector the action targets"* (`crates/newsroom/src/authz.rs:57-61`, and again at `authz.rs:285-289`).
- The **routes** are `/s/:sector/*` and resolve `:sector` to a desk (`crates/web/src/main.rs:101-127`); `/org/sectors` is the directory (`main.rs:66`).

**`meridian.desks` schema** (`migrations/0003_newsroom_structure.sql:95-113`):

```
id, name (UNIQUE), slug (UNIQUE), description,
lead_user_id → users, parent_id → desks (ON DELETE SET NULL),
settings jsonb DEFAULT '{}', is_archived, archived_at, created_at, updated_at
CONSTRAINT desks_name_length / desks_slug_length (1..120)
```

Note `name` and `slug` are **globally unique** and there is already a self-referential `parent_id` with an index (`desks_parent_idx`).

### 1.2 The desk hierarchy (`parent_id`) today

The `parent_id` tree is real and exercised, but only for **reporting roll-ups**, never for membership or authz:

- `create_desk(…, parent_id)` — *"Scoped to the parent: administering a sector carries the right to create sub-desks within it. A top-level desk is unscoped and org-level."* (`desks.rs:280-318`).
- `set_parent` / `update_desk` re-parent with a **cycle guard** (`desks.rs:326-375`, `521-562`).
- `descendant_ids` (BFS, cycle-safe), `ancestors`, `desk_tree`, `build_tree` (`desks.rs:449-514`, `1438-1454`).
- `desk_analytics(…, include_sub_desks)` rolls a desk up over `descendant_ids` (`desks.rs:1304-1379`).
- **Membership explicitly does not inherit down the tree** (`desks.rs:3-5`); `can_view_desk` and `list_desks` gate on a direct `desk_memberships` row (`desks.rs:1242-1254`, `251-273`).

So the tree exists structurally, but a child desk is a **full, independent workspace** — its own members, SLAs, schedule, invitations, branding, webhooks — not a lightweight label.

### 1.3 How a story attaches to a sector

- `Story.desk_id: Option<Uuid>` and `Story.category: String` are distinct fields (`crates/newsroom/src/stories.rs:122,129`); `StoryDraft`/`StoryPatch` carry both (`stories.rs:230-234,265-268`).
- **`desk_id` is the authorization boundary** for every editorial mutation:
  - `create_story` → `authz::require(…, "stories.create", draft.desk_id)` (`stories.rs:509`).
  - `update_story` → `authz::require(…, "stories.edit", story.desk_id)` then team check (`stories.rs:570-573`).
  - `delete_story`, `workflow.advance`, `workflow.fast_path`, `may_edit_story`/`stories.edit_any` all resolve against `story.desk_id` (`stories.rs:405,654,656,936,941`).
- **`category` is unstructured free text.** The seed sets it to sector-like strings (`'Politics'`, `'Business'`, `'Science & Health'`; `scripts/seed-newsroom.sql:139-161`) — i.e. today `category` duplicates the *sector* name, not an industry. The story list can filter by `desk_id` but not by category/industry (`stories.rs:367-379`).
- Separately, the platform carries the 17 **IPTC Media Topics** as a subject taxonomy for interchange feeds (`crates/contracts/src/lib.rs:62-89`) — orthogonal to sectors; not a candidate for the internal industry layer.

**Takeaway:** a story already points at one sector (`desk_id`). The industry layer is a *classification within that sector*, for which `category` is the only current (and inadequate) slot.

### 1.4 Applications flow (the `sector.application.*` surface)

Membership in a sector is granted, never self-served (`migrations/0024_sector_applications.sql:1-14`). The `sector_applications` table (`0024`, extended by `0025_kyc_certificate.sql`):

```
id, desk_id → desks (CASCADE), user_id → users (CASCADE),
role text DEFAULT 'reporter', status CHECK IN (pending|approved|rejected|withdrawn),
message, kyc_documents jsonb '[]', decision_note, reviewed_by, reviewed_at,
verification_signature / verification_alg / verification_pubkey (ML-DSA cert), created_at, updated_at
UNIQUE (desk_id, user_id) WHERE status='pending'   -- one open request per person per sector
```

Service methods (`crates/newsroom/src/desks.rs`):

| Method | Gate | Lines |
|---|---|---|
| `sectors_directory()` | open (browse/apply) | 957-963 |
| `apply_to_sector(desk_id, role, msg, kyc)` | any authenticated user; blocks existing members; one pending per (desk,user) | 967-1004 |
| `my_applications()` | self | 1010-1022 |
| `pending_applications()` | **global** `desks.manage` (scope `None`) | 1029-1043 |
| `review_application(id, approve, note)` | `desks.manage` **scoped to the application's desk** | 1050-1152 |
| `certificate(id)` | reconstruct + re-verify ML-DSA cert | 1167-1217 |

On approval, `review_application` inserts the same `desk_memberships` row an accepted invitation would (`desks.rs:1128-1139`) and signs a post-quantum KYC certificate (`desks.rs:1085-1108`). REST surface: `crates/web/src/newsroom_routes.rs:64-70`.

The parallel **invitation** path (lead/`desks.invite` → `desk_invitations` → membership on accept) lives at `desks.rs:778-947`.

### 1.5 Authorization model

**Capabilities** (`crates/newsroom/src/capabilities.rs`) are a static registry, deny-by-default; unregistered ⇒ denied to everyone (`capabilities.rs:1-6`). Each capability has a **`Scope`**:

- `Scope::Global` — answered once for the whole org (governance/flags/health).
- `Scope::Sector` — answered per sector, where a workspace membership is the grant (`capabilities.rs:71-83`).

Roles split into **four global roles** carried org-wide — `Admin`, `EditorInChief`, `StandardsLegal`, `AudienceEditor` (`capabilities.rs:85-102`) — and eight **workspace roles** held per membership. The desk capabilities:

- `desks.manage` — *"Create, edit, re-parent and archive desks (workspaces)."* — `Scope::Sector`, default audience `EDITORS` = `{Admin, EditorInChief, ManagingEditor, SectionEditor, StandardsLegal}` (`capabilities.rs:448-454`, `124-130`).
- `desks.invite` — same audience (`capabilities.rs:455-461`).
- Governance caps `permissions.manage`, `roles.manage`, `flags.manage`, `webhooks.manage`, `subscriptions.manage` — all `Scope::Global`, default `ADMIN_ONLY` (`capabilities.rs:463-497`).

**Resolver order** (`crates/newsroom/src/authz.rs:69-266`), first match wins: unregistered→deny · **admin flag→allow** (`authz.rs:85-92`) · user policy · active delegation (grant-only, **unscoped**) · active role assignment (**desk-scoped when `scope_type='desk'`**, `authz.rs:141-168`) · desk policy (deny beats allow) · global role default (answers when the cap is Global, the role is global, **or `desk_id.is_none()`**, `authz.rs:205-214`) · workspace-membership role for sector caps · else deny.

Two consequences that matter for this design:

1. **`is_admin` short-circuits everything** (`authz.rs:85-92`) — the admin can always manage any sector.
2. **An unscoped sector capability falls to the global role default** because `desk_id.is_none()` sets `global_answer = true` (`authz.rs:205-206`). So `desks.manage` asked with **`None`** (the top-level create/delete path) is granted to the whole `EDITORS` audience — *not* admin-only. This is the gap called out in §3.4.

**Per-desk admin short-circuit.** `require_desk_admin` lets a desk's own `lead_user_id` manage it without holding `desks.manage`; otherwise it requires `desks.manage` scoped to that desk (`desks.rs:417-432`). Used by `update_desk`, `set_parent`, `archive`, `replace_slas`, `set_schedule`.

### 1.6 Delegation machinery (`crates/newsroom/src/admin.rs`)

Three ways authority reaches a user, all audited, all guarded by the **escalation rule** (nobody may grant a capability they do not themselves hold — `admin.rs:282-294,792-811`):

| Mechanism | Table | Carries a desk scope? | Persistence | Code |
|---|---|---|---|---|
| **Role assignment** | `role_assignments (scope_type, scope_id)` | **Yes** — `scope_type='desk', scope_id=<desk>` | until `ends_at` | `assign_role` `admin.rs:583-630` |
| **Delegation** | `delegations (capabilities[])` | **No** — capability list only, granted org-wide | time-boxed window | `create_delegation` `admin.rs:669-705` |
| **Permission policy** | `permission_policies (subject user\|group)` | Group policy = a desk (subject_id is a desk_id) | until `expires_at` | `upsert_policy` `admin.rs:265-328` |

- Role definitions carry a `scope` (`global`/`workspace`) synced from the registry on boot by `sync_system_roles` (`admin.rs:363-393`); system roles cannot be edited/deleted (`admin.rs:485-489,525-540`).
- The seed already delegates per-sector: section editors get `role_assignments` with `scope_type='desk'` (`scripts/seed-newsroom.sql:113-121`).
- **Delegations cannot be desk-scoped** — the resolver grants a delegated capability regardless of `desk_id` (`authz.rs:119-137`). So a delegation of `desks.manage` would grant it in *every* sector — too broad for "one sector." Persistent single-sector delegation must therefore go through a **desk-scoped role assignment** (or desk lead), not `delegations`.

### 1.7 Where a sub-sector would surface in the UI/routes

The whole UI is single-level today: a `:sector` slug resolves to exactly one `desk_id` through `SectorScope::id_for_slug` (`crates/web/src/main.rs:218`), and `SectorView`/`SectorScope` (`main.rs:21376,197`) carry that id everywhere. Anchors:

- **Route grammar** (`crates/web/src/main.rs`): sector directory `/org/sectors` → `Workspaces` (`main.rs:66`); per-sector screens under `/s/:sector/*` — `SectorDashboard` (102-103), `Stories` (104-105), `SectorSettings` (110-111); admin-side `/admin/sectors` → `SectorAdmin` (150), `/admin/sectors/:sector_id/profile` → `SectorProfile` (156), `/admin/sector-applications` → `SectorApplications` (156). The `SectorSwitcher`/`SectorRail` (`main.rs:701,757`) are driven off `GET /api/v1/desks`.
- **Sector settings already composes per-sector panels.** `SectorSettings` (`main.rs:11980`) resolves `desk_id = scope.id_for_slug(&sector)` then renders SLAs/schedule/invitations, the desk **Hierarchy** view (`desk_tree`, `main.rs:12449`), and composes `SectorWebhooks` (`main.rs:11585`) and `SectorSmtp` (`main.rs:11794`) — the exact precedent for adding a **Sub-sectors + Policy** panel gated the same way.
- **Story editor desk picker.** The new-story composer `Editor()` (`main.rs:13192`) offers a `<select id="new-sector">` (`main.rs:13276-13281`, with an "Org level (no sector)" option) and submits `api::create_story(token, slug, title, desk_id)` (`main.rs:13217` → `POST /api/v1/stories` → handler `crates/web/src/story_routes.rs:606`). This `<select>` is where an **industry sub-picker** hangs, filtered to the chosen sector's sub-sectors. Note the story *edit* save (`save_story`) carries no desk/category field — desk binding is set only at create time.
- **REST** (`crates/web/src/newsroom_routes.rs`): desks CRUD `/api/v1/desks…` (37-48) backed by `create_desk` (`newsroom_routes.rs:419`) / `update_desk` (438); applications `/api/v1/sectors/*` + `/api/v1/sector-applications/*` (64-70); per-sector SMTP (84-89) and branding (81-82) — the precedent that a **desk lead configures the sector's own settings**.
- **Applications UI** already exists end-to-end: `ApplyToSectorPage` (`main.rs:18187`), `SectorBrowse` (18478), review queue `SectorApplications` (18566) calling `api::review_application`, and `MyApplications` (18108).

So a sub-sector layer surfaces in three known places — the `<select id="new-sector">` in the editor (industry sub-picker), the `SectorSettings` composition (a Sub-sectors + Policy panel beside `SectorWebhooks`/`SectorSmtp`), and the applications tab — all keyed on the existing `SectorScope`/`id_for_slug` desk resolution.

---

## 2. Proposed taxonomy model

### 2.1 The two levels

```
Sector            = meridian.desks            (unchanged: authz boundary, workspace, applications target)
  └─ Sub-sector   = meridian.sub_sectors      (NEW: classification within a sector)   ← "Industry"
        Story.desk_id       → its Sector      (unchanged authz boundary)
        Story.sub_sector_id → its Industry    (NEW, nullable; must belong to the story's Sector)
```

### 2.2 Recommendation — a dedicated `sub_sectors` table (not child desks)

**Recommended: Option B (dedicated table).** Reasons, each grounded:

1. **Membership does not inherit down `parent_id`** (`desks.rs:3-5`). If industries were child desks, a member of *Economy* would not be a member of child *Agriculture*, so `stories.create` scoped to the industry desk (`stories.rs:509`) would resolve via workspace-membership (`authz.rs:227-254`) and **deny** — nobody could file under an industry without a separate membership per industry. That directly contradicts "file under **any** industry."
2. **Slug collisions.** `desks.slug` is **globally unique** (`migrations/0003:98`). Industries like `coal`, `steel`, `power` recur across the taxonomy conceptually and would need globally unique slugs. The dedicated table scopes uniqueness per parent (`UNIQUE(desk_id, slug)`).
3. **Workspace-machinery explosion.** A desk drags SLAs, schedules, invitations, branding, SMTP, webhooks, analytics roll-ups, `lead_user_id`, and appears in `list_desks`/the sector switcher (`desks.rs:251-273`). ~30-40 industries × sectors would flood the switcher and every `desks`-scoped query with rows that should never be workspaces.
4. **Authz stays untouched.** Keeping the Sector (desk) as the boundary and the industry as an attribute means `Scope::Sector` resolution, `stories.edit_any`, desk-private reads, and the whole resolver are **unchanged** — the lowest-risk path. The industry is data on the story, not a new scope.

**Option A (child desks) — honest trade-offs.** It needs *zero* new tables and reuses `desk_tree`/`descendant_ids`; and `create_desk` already lets a **desk-scoped `desks.manage`** holder create sub-desks within a parent (`desks.rs:280-291`), which is exactly the delegation shape we want. Choose Option A **only if** the product decides an industry is a true sub-workspace with its own members, SLAs, and analytics. The requirement ("an article can be filed under any industry", applications and management at the *sector* level) says it is a **filed-under label**, so Option B wins.

### 2.3 Proposed data model (DDL sketch — for a future migration, not applied here)

```sql
-- migrations/00xx_sub_sectors.sql  (illustrative)
CREATE TABLE meridian.sub_sectors (
    id          uuid PRIMARY KEY,
    desk_id     uuid NOT NULL REFERENCES meridian.desks(id) ON DELETE CASCADE,  -- parent Sector
    name        text NOT NULL,
    slug        text NOT NULL,
    description text,
    position    int  NOT NULL DEFAULT 0,           -- display order within the sector
    is_archived boolean NOT NULL DEFAULT false,
    settings    jsonb NOT NULL DEFAULT '{}'::jsonb, -- optional per-industry policy override
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (desk_id, slug),
    UNIQUE (desk_id, name),
    CONSTRAINT sub_sectors_name_length CHECK (length(name) BETWEEN 1 AND 120)
);
CREATE INDEX sub_sectors_desk_idx ON meridian.sub_sectors (desk_id);

-- Story references its industry; keeps desk_id (the Sector / authz boundary).
ALTER TABLE meridian.stories
  ADD COLUMN sub_sector_id uuid REFERENCES meridian.sub_sectors(id) ON DELETE SET NULL;
CREATE INDEX stories_sub_sector_idx ON meridian.stories (sub_sector_id);
```

**Invariant — an industry belongs to the story's sector.** `sub_sector.desk_id` must equal `story.desk_id`. Enforce in `create_story`/`update_story` (a lookup already loads the story's desk, `stories.rs:509,570`); optionally back it with a composite FK (`stories(desk_id, sub_sector_id)` → `sub_sectors(desk_id, id)`) if the DB should reject a mismatch outright. The story **keeps `desk_id`** — authz never keys on the industry.

**`category` coexistence.** Leave `stories.category` in place (non-breaking). Longer term either (a) derive/display `category` from the chosen sub-sector, or (b) deprecate the free-text field. Do not overload `category` to store the industry — it is unstructured and unindexed.

### 2.4 De-duplicated grouping of the 40 industries

The seed list is recognisably a **Government-of-India ministry/PIB-style sector list**; it contains one exact duplicate, several nested/partial duplicates, and a handful of legitimately cross-cutting industries. It resolves to **~30 de-duplicated industries under 9 top-level Sectors**.

**Overlaps and duplicates to call out (as requested):**

| Kind | Raw entries | Resolution |
|---|---|---|
| **Exact duplicate** | *Animal Husbandry, Dairying & Fisheries* (#3) = *Fisheries, Animal Husbandry and Dairying* (#20) | Collapse to one: **Fisheries, Animal Husbandry & Dairying** |
| **Nested / partial** | *Fertilizers* (#19) ⊂ *Chemicals & Fertilizers & Minerals* (#7); *Petrochemicals* (#8) ⊂ Chemicals | Collapse to one: **Chemicals, Petrochemicals & Fertilizers** |
| **Nested / partial** | *Minerals* (inside #7) ↔ *Mines* (#29) | Fold Minerals into **Mines & Minerals** |
| **Overlap** | *Consumer Brands* (#12) ↔ *FMCG* (#21) | Collapse to one: **Consumer Goods (FMCG)** |
| **Overlap** | *Manufacturing* (#28) ↔ *Heavy Industries* (#24) | Keep both as distinct industries under one sector (breadth vs heavy plant) |
| **Overlap** | *Technology* (#38) ↔ *Electronics, IT & Components* (#16) ↔ *AI & Cyber Security* (#2) | Keep as distinct industries under one Sci-Tech sector |
| **Umbrella vs member** | *Energy & Sustainability* (#17) / *Power* (#34) overlap the Energy sector name | Treat **Energy & Sustainability** as the sector umbrella; keep **Power** as an industry |
| **Cross-cutting (dual)** | *Coal* (#10) Energy ↔ Mining; *Atomic Energy* (#4) Energy ↔ Science; *Space* (#36) Science ↔ Defence; *Biotechnology* (#6) Sci-Tech ↔ Health | One **canonical** parent each (below); cross-listing handled by tags, not a second parent (see §7) |

**Proposed top-level Sectors → Sub-sectors (industries):**

| # | Top-level Sector | Sub-sectors (industries) | Nearest current desk (§1.1 / seed) |
|---|---|---|---|
| 1 | **Agriculture & Rural** | Agriculture · Fisheries, Animal Husbandry & Dairying · Food Processing Industries | *(new; adjacent to Business & Economy)* |
| 2 | **Economy, Industry & Manufacturing** | Manufacturing · Heavy Industries · Steel · Textiles · Chemicals, Petrochemicals & Fertilizers · Mines & Minerals · Consumer Goods (FMCG) · Services · Labour & Employment | Business & Economy (`business`) |
| 3 | **Energy** | Power · Coal · Petroleum & Natural Gas · New & Renewable Energy · Atomic Energy | *(new; adjacent to Business/Science)* |
| 4 | **Science, Technology & Communications** | Technology · AI & Cyber Security · Electronics, IT & Components · Communications · Biotechnology · Earth Sciences | Technology (`technology`) |
| 5 | **Health** | Health & Family Welfare · Pharmaceuticals · Ayush, Ayurveda & Herbal Medicine | Science & Health (`science`) |
| 6 | **Environment & Climate** | Environment, Forest & Climate Change | Science & Health (`science`) |
| 7 | **Infrastructure & Transport** | Infrastructure & Construction · Civil Aviation · Ports, Shipping & Waterways | *(new)* |
| 8 | **Defence, Aerospace & Space** | Defence & Aerospace · Space | *(new; adjacent to World/Investigations)* |
| 9 | **Governance & Society** | Education · Information & Broadcasting · Tourism | Politics (`politics`) / World (`world`) |

This yields **30 distinct industries** from the 40 raw entries (10 removed as duplicates/nested/umbrella). The mapping is a **proposal for review**, not a schema constant — the industry set lives in data (`sub_sectors` rows), so editors can refine it without a code change. Cross-cutting industries (*Coal, Atomic Energy, Space, Biotechnology*) take a single canonical parent above; §7 covers whether to add optional secondary tags.

---

## 3. Governance & access control

### 3.1 Capability matrix

| Action | Required capability | Scope passed to `authz::require` | Who satisfies it |
|---|---|---|---|
| **Create top-level Sector** | `sectors.admin` *(new, Global)* | `None` | Admin only (default `ADMIN_ONLY`) |
| **Delete top-level Sector** | `sectors.admin` *(new, Global)* | `None` | Admin only |
| **Edit Sector** (name/desc/lead/parent) | `desks.manage` (or desk lead) | `Some(sector)` | Admin · sector lead · desk-scoped `desks.manage` grantee |
| **Archive / unarchive Sector** | `desks.manage` (or desk lead) | `Some(sector)` | same |
| **Create Sub-sector** | `desks.manage` (or desk lead) | `Some(parent_sector)` | Admin · delegated sector manager |
| **Edit Sub-sector** | `desks.manage` (or desk lead) | `Some(parent_sector)` | same |
| **Delete / archive Sub-sector** | `desks.manage` (or desk lead) | `Some(parent_sector)` | same |
| **Edit Sector policy** (`settings`) | `desks.manage` (or desk lead) | `Some(sector)` | same |
| **List a sector's applications** *(new endpoint)* | `desks.manage` | `Some(sector)` | Admin · that sector's manager |
| **List the org-wide pending queue** | `desks.manage` | `None` (existing `pending_applications`) | Admin / org `EDITORS` |
| **Review one application** | `desks.manage` (existing `review_application`) | `Some(app.desk_id)` | Admin · that sector's manager |
| **Assign a sector manager** | `roles.manage` (+ escalation guard) | `None` (Global, `ADMIN_ONLY`) | Admin |
| **Grant a per-sector policy** | `permissions.manage` | `None` (Global, `ADMIN_ONLY`) | Admin |

Everything except the two new rows (`sectors.admin`; a per-sector applications *list*) already exists and works today; sub-sector CRUD reuses `desks.manage Some(parent)` exactly as `create_desk` already does for sub-desks (`desks.rs:280-291`).

### 3.2 Delegating full management of one sector (the core requirement)

"A single delegated user should fully manage one sector." Two supported paths, both existing:

1. **Desk-scoped role assignment (recommended, persistent).** Assign the user a role that includes `desks.manage` (and `desks.invite`) with `scope_type='desk', scope_id=<sector>` via `assign_role` (`admin.rs:583-630`). The resolver honours the scope — a `desk` assignment matches only the targeted desk (`authz.rs:153-168`). This is exactly the shape the seed already uses for section editors (`scripts/seed-newsroom.sql:113-121`). That single grant then satisfies: edit/archive/re-parent the sector, set SLAs and schedule, invite members, create/edit/delete its sub-sectors, edit its policy, and review its applications — because every one of those calls `desks.manage Some(sector)` or `require_desk_admin` (§1.5, §3.1).

2. **Desk lead.** Set `desks.lead_user_id = <user>`; `require_desk_admin` short-circuits for the lead (`desks.rs:417-432`). Simpler, but only covers the `require_desk_admin` call-sites, and lead-ness is a single-user row-level fact, not a grantable capability — prefer path 1 for auditable delegation.

**Escalation guard.** To assign a `desks.manage` role, the granting admin must themselves hold `desks.manage` (checked unscoped via `check_can_package` → `authz::has(…, None)`, `admin.rs:596,803`). The admin flag satisfies this (`authz.rs:85-92`).

**Not a fit: `delegations`.** A `delegations` row is org-wide (`authz.rs:119-137`) — delegating `desks.manage` this way would grant it in *every* sector. Use it only for genuinely cross-sector, time-boxed cover, never for single-sector management.

### 3.3 Keeping global add/remove admin-only

Introduce **`sectors.admin`** — a `Scope::Global` capability defaulting to `ADMIN_ONLY` — and gate top-level `create_desk(parent_id=None)` and `delete_desk` of a root sector on it. Because it is Global + `ADMIN_ONLY`, only the admin flag (or an explicit admin-granted policy) passes, and — unlike `desks.manage None` — it is **not** reachable by the `desk_id.is_none()` global-default path for the `EDITORS` audience (§3.4). Sub-sector and per-sector actions stay on desk-scoped `desks.manage`, preserving delegation. (Registry additions are compile-time constants in `capabilities.rs`; `sync_system_roles` mirrors them to `role_definitions` on boot — `admin.rs:363-393`.)

### 3.4 Gap found in the current model

Today `desks.manage` is `Scope::Sector` with audience `EDITORS`. When `create_desk`/`delete_desk` are called for a **top-level** desk, `desk_id` is `None`, so the resolver's global-default branch fires (`authz.rs:205-214`) and grants the action to the **entire `EDITORS` audience** (`Admin, EditorInChief, ManagingEditor, SectionEditor, StandardsLegal`) — not admin-only. This means a section editor can currently create or delete a top-level sector. The requirement wants global add/remove restricted to admin (or an explicit grant). §3.3's `sectors.admin` closes this precisely; alternatively, tighten the unscoped create/delete audience — but that is a behavior change for existing editor workflows and should be a conscious product decision (see §7).

---

## 4. What "sector policy" is, and where it lives

**Definition.** A *sector policy* is the governing configuration of a sector: its editorial rules, who may write in it, what an application must supply, and its service-level expectations. It is per-sector, editable by that sector's manager.

**Home.** The desk's existing `settings jsonb` column — documented as "the desk's registry profile (policy, documents, metadata) as a jsonb blob… `Some(value)` replaces it wholesale" (`crates/newsroom/src/desks.rs:158-160`), written through `DeskPatch.settings` in `update_desk` (`desks.rs:351,359`). No new table is required for policy. Proposed shape under a reserved `policy` key:

```jsonc
// desks.settings.policy
{
  "editorial_rules":      "Markdown/plaintext house rules for this sector.",
  "writable_by_roles":    ["reporter", "section_editor"],   // roles that may file here
  "membership":           "application",                     // "open" | "application" | "invite_only"
  "application_requirements": {
    "kyc_documents":      ["press_card", "id_proof"],        // labels the applicant must attach
    "cover_note_required": true
  },
  "default_slas": [ { "workflow_state": "verification", "target_hours": 24, "warn_at_percent": 80 } ]
}
```

Notes: `default_slas` is advisory metadata; the enforced SLAs remain the typed `desk_slas` rows (`desks.rs:643-705`). `writable_by_roles`/`membership` are policy the application UI and (optionally) `create_story` can read; enforcement stays server-side through the existing `Scope::Sector` resolution. A sub-sector may carry a narrow override in `sub_sectors.settings` (e.g. a stricter industry) resolved over the parent policy.

---

## 5. Applications flow (join / write in a sector)

The existing `sector.application.*` flow (§1.4) already implements exactly what the ask describes, at the **sector** level — keep it, extend lightly:

1. **Discover** — user browses `sectors_directory()` (`/api/v1/sectors/directory`, `desks.rs:957`).
2. **Apply** — `apply_to_sector(desk_id, role, message, kyc_documents)` files a pending application; the sector policy's `application_requirements` (§4) drives which KYC labels the form asks for (`desks.rs:967-1004`).
3. **Optional industry hint** — add a nullable `sub_sector_id` to `sector_applications` so an applicant can indicate the industry they intend to cover. Membership stays at the sector, so this is informational routing, not a second authz level.
4. **Review** — the sector's manager approves/rejects: `review_application` is already scoped to the application's desk (`desks.rs:1065`), so a desk-scoped `desks.manage` delegate handles its own sector's queue. Add a **per-sector list** endpoint (`desks.manage Some(sector)`) so a delegate sees only their sector's pending items; the org-wide `pending_applications` (`None`) stays for admins/org editors (`desks.rs:1029-1043`).
5. **Grant** — approval inserts the `desk_memberships` row and signs the ML-DSA KYC certificate (`desks.rs:1128-1139,1085-1108`); the applicant can re-verify it (`certificate`, `desks.rs:1167-1217`).

Who approves: **admin, the sector's manager (desk-scoped `desks.manage`), or the desk lead** — the same set that manages the sector. No change to the approval authz is required.

---

## 6. Migration & rollout plan (phased, non-breaking)

**Phase 1 — schema + capabilities (additive).** New migration `sub_sectors` table + nullable `stories.sub_sector_id` (§2.3). Register `sectors.admin` (Global, `ADMIN_ONLY`) and — optionally — a named `sector.manage` alias for `desks.manage` in `capabilities.rs`; `sync_system_roles` mirrors new caps to `role_definitions` on boot (`admin.rs:363-393`). Nothing existing changes: `desk_id`, `category`, and all authz keep working.
  - *Ops caveats (from project memory):* the `migrate` binary embeds migrations at compile time — force-rebuild before migrating or the new file is silently skipped. After the `ALTER TABLE meridian.stories`, **restart `meridian-pgbouncer`** or queries 500 with "cached plan must not change result type."

**Phase 2 — backfill.** Seed `sub_sectors` for existing desks from the §2.4 grouping. Existing stories' free-text `category` holds *sector* names (`'Politics'`, `'Business'`; seed:139-161), **not** industries, so there is nothing to auto-map to `sub_sector_id` — leave it `NULL` and let editors classify going forward (or run a one-off editorial pass). The 8 seeded desks map onto Sectors #2/#4/#5/#6/#9 in §2.4; new top-level sectors (Agriculture, Energy, Infrastructure, Defence) are created as needed.

**Phase 3 — service + API.** `create_sub_sector` / `update_sub_sector` / `delete_sub_sector` (each `authz::require("desks.manage", Some(parent))`); `list_sub_sectors(sector)`; a per-sector applications list; thread `sub_sector_id` through `StoryDraft`/`StoryPatch` and `create_story`/`update_story` with the "industry belongs to the story's sector" invariant (§2.3). Top-level create/delete switch to `sectors.admin` (§3.3).

**Phase 4 — UI.** Industry picker in the story editor; a Sub-sectors + Policy panel in `/s/:sector/settings` (`main.rs:110-111`), gated on the same `desks.manage Some(sector)` the SMTP/branding panels already use (`newsroom_routes.rs:81-89`); applications tab in sector settings.

**Phase 5 — webhooks / contracts (additive).** Extend the catalogue in `crates/contracts/src/lib.rs` (`WEBHOOK_EVENT_GROUPS`, currently 124 events across the Desks group at `lib.rs:329-348` and Sector-applications group at `lib.rs:349-357`):
  - New **"Sub-sectors"** group, `resource = "subsector"`: `subsector.created`, `subsector.updated`, `subsector.archived`, `subsector.deleted`.
  - Sector policy: `sector.policy.updated` (a distinct event beside `desk.updated`).
  - **Contract gate:** the `catalogue_is_well_formed` test enforces that every event is prefixed by its group `resource.` and that types are unique (`lib.rs:470-497`) — new events must follow `subsector.*`. Adding to the catalogue is forward-compatible: subscriptions may select an event before it is emitted and simply receive nothing until wired (`lib.rs:133-139`).
  - **Emission:** emit via `enqueue_event(tx, "subsector.created", <id>, Some(<parent sector slug>), payload, actor)` inside the domain transaction (`crates/newsroom/src/webhooks.rs:532-557`); passing the parent sector slug as the `desk` lets desk-scoped webhook endpoints (migration `0038_webhook_desk_scope.sql`) route industry events to the right sector's subscribers.

**Rollback.** Each phase is independently revertible; because `sub_sector_id` is nullable and `desk_id`/`category`/authz are untouched, dropping the column and the table restores the prior behavior with no data loss to stories.

---

## 7. Open questions & risks

1. **Required vs optional industry.** Is `sub_sector_id` mandatory on new stories, or optional (nullable, as proposed)? Could be enforced per-sector via `policy.membership`/a `require_sub_sector` flag rather than globally.
2. **Cross-listed industries.** *Coal, Atomic Energy, Space, Biotechnology* legitimately span two sectors. The proposal gives each a single canonical parent (strict tree). If the newsroom needs many-to-many filing, add an optional `story_sub_sectors` join or a secondary-tag list — but that complicates roll-ups and search; defer until demanded.
3. **Tightening top-level create/delete is a behavior change.** Introducing `sectors.admin` (or narrowing the unscoped `desks.manage` audience) removes a power section editors have today (§3.4). Confirm with the product owner; audit existing usage first.
4. **`category` future.** Keep the free-text column, derive it from the sub-sector, or deprecate it? It currently duplicates the sector name and is unindexed for filtering.
5. **Delegations can't be desk-scoped** (`authz.rs:119-137`). If the product wants *time-boxed* single-sector cover (not a standing role), we would need to add scope to `delegations` — a resolver + schema change. Otherwise single-sector delegation is role-assignment-only (§3.2).
6. **Slug strategy for industries.** Per-sector unique slugs (`UNIQUE(desk_id, slug)`) mean the same industry slug can recur across sectors; URLs must therefore be sector-qualified (e.g. `/s/economy/i/steel`) rather than a global `/industry/steel`.
7. **Membership granularity.** This design keeps membership at the sector. If reporters should be scoped to *specific industries* within a sector (read/write only their industries), that is a materially larger authz change (a new `Scope`), and pushes toward Option A (child desks). Decide before Phase 1.
8. **Reviewer visibility.** Adding a per-sector applications list (§5) must not leak other sectors' applicants; scope every query by the delegate's `desks.manage Some(sector)` set, mirroring the desk-private read model already enforced elsewhere (`desks.rs:1274-1302`).

---

*Ends. This is a design document; no code, migration, or configuration has been changed.*
