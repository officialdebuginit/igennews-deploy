# iGEN News — accounts, roles & capabilities

All 35 accounts seeded by `scripts/seed-igennews.sql`. **Every account logs in at
`/sign-in` with the same password: `DevPass123!`** (username = the email below, or the
handle). Accounts exist only after you run the seed:

```bash
set -a; source .env.local; set +a
SEED_DATABASE_URL="$MIGRATION_DATABASE_URL" scripts/seed-all.sh
```

> Use the **owner/migrator** URL, not `DATABASE_DIRECT_URL`. The seed opens with
> `TRUNCATE`, which the application role is not granted, so seeding as the app user
> fails on the first statement. `seed-all.sh` checks this before writing anything.

> Org-wide **role** (`users.role`) is the person's standing across the newsroom.
> **Capabilities** are what a role can *do*; they resolve per-sector, except the
> super-admin whose `is_admin` flag short-circuits every check everywhere.
>
> Each account also ships a **complete profile** (seed section 2b): a bio, phone,
> `Asia/Kolkata` timezone, languages (English + Hindi + a regional language by
> location), role/beat-specific skills, availability status and newsroom visibility.

---

## Masthead & standards (org-wide roles)

| Name | Login (email) | Handle | Role | Title |
|---|---|---|---|---|
| Sanjay Shah | `admin@igennews.com` | `admin` | **admin (super-admin)** | Owner / Super Administrator |
| Ananya Rao | `arao@igennews.com` | `a.rao` | editor_in_chief | Editor-in-Chief |
| Vikram Iyer | `viyer@igennews.com` | `v.iyer` | managing_editor | Managing Editor |
| Neha Desai | `ndesai@igennews.com` | `n.desai` | standards_legal | Standards & Legal Counsel |
| Rahul Pillai | `rpillai@igennews.com` | `r.pillai` | fact_checker | Chief Fact-Checker |
| Sneha Banerjee | `sbanerjee@igennews.com` | `s.banerjee` | copy_editor | Chief Sub-Editor |
| Kavya Menon | `kmenon@igennews.com` | `k.menon` | audience_editor | Head of Audience & SEO |
| Thomas George | `tgeorge@igennews.com` | `t.george` | producer | Digital Producer |

## Section editors (desk-scoped)

| Name | Login (email) | Handle | Role | Sectors led |
|---|---|---|---|---|
| Arjun Kapoor | `etech@igennews.com` | `e.tech` | section_editor | AI & Cyber Security · Semiconductors · Technology |
| Priya Nair | `efin@igennews.com` | `e.fin` | section_editor | Banking & Financial Services · FinTech & Digital Payments · Startups & Innovation |
| Anjali Rao | `ehealth@igennews.com` | `e.health` | section_editor | Health and Family Welfare · Pharmaceutical |
| Deepak Sharma | `eenergy@igennews.com` | `e.energy` | section_editor | Energy & Sustainability · New and Renewable Energy |
| Sanjana Bose | `eauto@igennews.com` | `e.auto` | section_editor | Automotive & Electric Vehicles |
| Rajesh Singh | `edef@igennews.com` | `e.def` | section_editor | Defence & Aerospace · Space |
| Lakshmi Iyer | `eagri@igennews.com` | `e.agri` | section_editor | Agriculture |
| Suresh Gupta | `einfra@igennews.com` | `e.infra` | section_editor | Railways & Metro · Infrastructure & Construction |
| Ritu Agarwal | `ebiz@igennews.com` | `e.biz` | section_editor | Retail & E-Commerce · Tourism |

## Reporters (desk-scoped)

| Name | Login (email) | Handle | Role | Sector |
|---|---|---|---|---|
| Meera Krishnan | `rai@igennews.com` | `r.ai` | reporter | AI & Cyber Security |
| Rohan Verma | `rchip@igennews.com` | `r.chip` | reporter | Semiconductors |
| Isha Malhotra | `rtech@igennews.com` | `r.tech` | reporter | Technology |
| Aditya Ghosh | `rbank@igennews.com` | `r.bank` | reporter | Banking & Financial Services |
| Zara Sheikh | `rfintech@igennews.com` | `r.fintech` | reporter | FinTech & Digital Payments |
| Karan Mehta | `rstartup@igennews.com` | `r.startup` | reporter | Startups & Innovation |
| Sana Qureshi | `rhealth@igennews.com` | `r.health` | reporter | Health and Family Welfare |
| Nikhil Joshi | `rpharma@igennews.com` | `r.pharma` | reporter | Pharmaceutical |
| Fatima Ansari | `renergy@igennews.com` | `r.energy` | reporter | Energy & Sustainability |
| Aarav Reddy | `rrenew@igennews.com` | `r.renew` | reporter | New and Renewable Energy |
| Vivek Nair | `rauto@igennews.com` | `r.auto` | reporter | Automotive & Electric Vehicles |
| Ayaan Khan | `rdef@igennews.com` | `r.def` | reporter | Defence & Aerospace |
| Tara Menon | `rspace@igennews.com` | `r.space` | reporter | Space |
| Manav Patel | `ragri@igennews.com` | `r.agri` | reporter | Agriculture |
| Divya Rao | `rrail@igennews.com` | `r.rail` | reporter | Railways & Metro |
| Harsh Vardhan | `rinfra@igennews.com` | `r.infra` | reporter | Infrastructure & Construction |
| Pooja Shetty | `rretail@igennews.com` | `r.retail` | reporter | Retail & E-Commerce |
| Aisha Sheikh | `rtourism@igennews.com` | `r.tourism` | reporter | Tourism |

Rahul Pillai (fact-checker) and Sneha Banerjee (copy-editor) also hold desk
memberships on AI & Cyber Security, Banking & Financial Services and Defence &
Aerospace, so they work across those desks in addition to their org-wide roles.

---

## What each role can do (capabilities)

Capabilities come from the registry in `crates/newsroom/src/capabilities.rs` and
resolve **per sector** (except the super-admin). Summary:

| Role | Key capabilities |
|---|---|
| **admin** (super-admin) | Everything, everywhere — `is_admin` short-circuits all checks. The only role that holds the admin-only governance caps: `sectors.admin`, `permissions.manage`, `roles.manage`, `flags.manage`, `webhooks.manage`, `subscriptions.manage`. |
| **editor_in_chief** | Full editorial authority across all sectors: manage desks & invites, commission pitches, advance workflow, edit **any** story (`stories.edit_any`), publish releases, approve/reject reviews. Not the admin-only governance caps. |
| **managing_editor** | Day-to-day operations across desks: desk management, workflow advance, publishing, reviews, task/coverage management. |
| **section_editor** | Everything an editor does **within their own sector(s)**: manage that desk, invite, commission pitches, advance workflow, edit any story in the desk, publish. Scoped to their desk(s) only. |
| **standards_legal** | The standards/legal review gate in the publish path; comments; can hold/return stories on standards grounds. |
| **audience_editor** | Audience & analytics: front-page curation, feed moderation, SEO/distribution surfaces. |
| **fact_checker** | Verification: record claims & evidence, decide claims (`claims.decide`), and sign off the fact-check review step. |
| **copy_editor** | The copy/standards review step; comments and corrections. |
| **producer** | Production & release scheduling of approved stories. |
| **reporter** | Create and edit **their own** stories, submit for review, add sources/evidence, file pitches, and file under a sector + industry. Cannot publish or manage a desk. |

Role → capability audiences are enforced server-side; the sidebar and controls only
show what the signed-in account can actually do.
