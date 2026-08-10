-- Real-world newsroom seed for "The Meridian".
--
-- Clears all accumulated demo/test data (parity probes, test desks) and rebuilds a
-- realistic newsroom: a masthead with proper roles, eight section desks, and stories
-- spread across every workflow state, plus pitches, tasks and planned coverage.
--
-- Every seeded user logs in with the same password as admin (DevPass123!) — the hash
-- below is reused verbatim so the accounts are immediately usable for a walkthrough.
--
-- Run:  psql "$DATABASE_DIRECT_URL" -v ON_ERROR_STOP=1 -f scripts/seed-newsroom.sql

BEGIN;

-- 1. Wipe demo data (keep schema + reference tables: role_definitions, channels,
--    feature_flags, migration ledgers). CASCADE clears every dependent row.
TRUNCATE
  meridian.users, meridian.desks, meridian.stories, meridian.pitches, meridian.tasks,
  meridian.coverage_events, meridian.desk_memberships, meridian.role_assignments,
  meridian.reviews, meridian.releases, meridian.release_attempts, meridian.corrections,
  meridian.story_versions, meridian.evidence, meridian.claims, meridian.comments,
  meridian.approvals, meridian.sources, meridian.source_access_grants, meridian.assets,
  meridian.drive_folders, meridian.notifications, meridian.notification_preferences,
  meridian.attention_states, meridian.dashboard_views, meridian.saved_searches,
  meridian.follows, meridian.favorites, meridian.recent_items, meridian.presence,
  meridian.feed_posts, meridian.feed_replies, meridian.feed_likes, meridian.feed_reposts,
  meridian.desk_invitations, meridian.desk_schedules, meridian.desk_slas,
  meridian.sector_applications, meridian.legal_documents, meridian.legal_document_parties,
  meridian.permission_policies, meridian.delegations, meridian.sessions,
  meridian.password_reset_tokens, meridian.audit_events, meridian.workflow_state_history,
  meridian.metric_snapshots, meridian.front_page_slots, meridian.workspace_branding
  RESTART IDENTITY CASCADE;

-- 2. People — the masthead. Shared password hash = DevPass123!.
INSERT INTO meridian.users (id, email, handle, display_name, password_hash, role, department, title, is_admin, location, pronouns)
SELECT gen_random_uuid(), v.email, v.handle, v.display_name,
       '$argon2id$v=19$m=19456,t=2,p=1$qPd6IKfoz3jFz8B2h6UA1A$FOOpr4hXbo8/60MsN3ekPPiHR3sgYpNCgZX4sGqPH5Q',
       v.role, v.department, v.title, v.is_admin, v.location, v.pronouns
FROM (VALUES
  ('admin@meridian.example',   'admin',      'Sanjay Shah',      'admin',           'Masthead',        'Owner / Administrator',   true,  'London',      'he/him'),
  ('e.mercer@meridian.example', 'emercer',   'Evelyn Mercer',    'editor_in_chief', 'Masthead',        'Editor-in-Chief',         false, 'London',      'she/her'),
  ('d.okafor@meridian.example', 'dokafor',   'David Okafor',     'managing_editor', 'Masthead',        'Managing Editor',         false, 'London',      'he/him'),
  ('l.chen@meridian.example',   'lchen',     'Lily Chen',        'section_editor',  'Politics',        'Politics Editor',         false, 'Washington',  'she/her'),
  ('r.malik@meridian.example',  'rmalik',    'Rohan Malik',      'section_editor',  'Business',        'Business Editor',         false, 'New York',    'he/him'),
  ('s.rossi@meridian.example',  'srossi',    'Sofia Rossi',      'section_editor',  'Technology',      'Technology Editor',       false, 'San Francisco','she/her'),
  ('t.andersen@meridian.example','tandersen','Thomas Andersen',  'section_editor',  'World',           'World Editor',            false, 'Berlin',      'he/him'),
  ('n.abara@meridian.example',  'nabara',    'Nadia Abara',      'section_editor',  'Science & Health','Science & Health Editor', false, 'Nairobi',     'she/her'),
  ('j.kim@meridian.example',    'jkim',      'Jae Kim',          'section_editor',  'Culture',         'Culture Editor',          false, 'Seoul',       'they/them'),
  ('m.flores@meridian.example', 'mflores',   'Marcus Flores',    'section_editor',  'Sports',          'Sports Editor',           false, 'Madrid',      'he/him'),
  ('h.bennett@meridian.example','hbennett',  'Harriet Bennett',  'section_editor',  'Investigations',  'Investigations Editor',   false, 'London',      'she/her'),
  ('a.oconnor@meridian.example','aoconnor',  'Aoife O''Connor',  'reporter',        'Politics',        'Political Correspondent', false, 'Washington',  'she/her'),
  ('b.wright@meridian.example', 'bwright',   'Ben Wright',       'reporter',        'Business',        'Markets Reporter',        false, 'New York',    'he/him'),
  ('y.tanaka@meridian.example', 'ytanaka',   'Yuki Tanaka',      'reporter',        'Technology',      'Technology Reporter',     false, 'Tokyo',       'she/her'),
  ('i.mueller@meridian.example','imueller',  'Ingrid Müller',    'reporter',        'World',           'Foreign Correspondent',   false, 'Kyiv',        'she/her'),
  ('p.nguyen@meridian.example', 'pnguyen',   'Priya Nguyen',     'reporter',        'Science & Health','Health Reporter',         false, 'Singapore',   'she/her'),
  ('g.santos@meridian.example', 'gsantos',   'Gabriel Santos',   'reporter',        'Sports',          'Sports Reporter',         false, 'Rio de Janeiro','he/him'),
  ('f.khan@meridian.example',   'fkhan',     'Farah Khan',       'fact_checker',    'Standards',       'Chief Fact-Checker',      false, 'London',      'she/her'),
  ('l.petrov@meridian.example', 'lpetrov',   'Luka Petrov',      'copy_editor',     'Standards',       'Chief Sub-Editor',        false, 'London',      'he/him'),
  ('m.reyes@meridian.example',  'mreyes',    'Maria Reyes',      'standards_legal', 'Standards & Legal','Standards & Legal Counsel',false,'London',      'she/her'),
  ('c.dubois@meridian.example', 'cdubois',   'Camille Dubois',   'audience_editor', 'Audience',        'Head of Audience',        false, 'Paris',       'she/her')
) AS v(email, handle, display_name, role, department, title, is_admin, location, pronouns);

-- 3. Desks — the eight sections. Lead set by handle.
INSERT INTO meridian.desks (id, name, slug, description, lead_user_id, settings, is_archived, created_at, updated_at)
SELECT gen_random_uuid(), v.name, v.slug, v.description, u.id, '{}'::jsonb, false, now(), now()
FROM (VALUES
  ('Politics',         'politics',        'Government, elections, policy and power.',              'lchen'),
  ('Business & Economy','business',       'Markets, companies, labour and the economy.',           'rmalik'),
  ('Technology',       'technology',      'Tech, AI, platforms and their consequences.',           'srossi'),
  ('World',            'world',           'Foreign affairs, conflict and diplomacy.',              'tandersen'),
  ('Science & Health', 'science',         'Research, medicine, climate and the environment.',      'nabara'),
  ('Culture',          'culture',         'Film, music, books, art and ideas.',                    'jkim'),
  ('Sports',           'sports',          'Football, athletics and the business of sport.',        'mflores'),
  ('Investigations',   'investigations',  'Long-form accountability reporting.',                   'hbennett')
) AS v(name, slug, description, lead_handle)
JOIN meridian.users u ON u.handle = v.lead_handle;

-- 4. Desk memberships (desk-level role from desk_memberships_role_check).
INSERT INTO meridian.desk_memberships (desk_id, user_id, role, joined_at)
SELECT d.id, u.id, v.role, now()
FROM (VALUES
  ('politics','lchen','section_editor'),   ('politics','aoconnor','reporter'),
  ('business','rmalik','section_editor'),  ('business','bwright','reporter'),
  ('technology','srossi','section_editor'),('technology','ytanaka','reporter'),
  ('world','tandersen','section_editor'),  ('world','imueller','reporter'),
  ('science','nabara','section_editor'),   ('science','pnguyen','reporter'),
  ('culture','jkim','section_editor'),
  ('sports','mflores','section_editor'),   ('sports','gsantos','reporter'),
  ('investigations','hbennett','section_editor'), ('investigations','aoconnor','reporter'),
  -- standards & masthead sit across desks
  ('politics','fkhan','fact_checker'),     ('business','fkhan','fact_checker'),
  ('investigations','fkhan','fact_checker'),('politics','lpetrov','copy_editor'),
  ('world','lpetrov','copy_editor'),       ('politics','dokafor','managing_editor'),
  ('business','dokafor','managing_editor')
) AS v(desk_slug, handle, role)
JOIN meridian.desks d ON d.slug = v.desk_slug
JOIN meridian.users u ON u.handle = v.handle;

-- 5. Role assignments. Global masthead roles + desk-scoped section roles, so the
--    permission model (and the Roles admin screen) reflects a real org chart.
INSERT INTO meridian.role_assignments (id, user_id, role_key, scope_type, scope_id, starts_at, granted_by_id, reason)
SELECT gen_random_uuid(), u.id, v.role_key, 'global', NULL, now(), a.id, v.reason
FROM (VALUES
  ('emercer','editor_in_chief','Runs the newsroom'),
  ('dokafor','managing_editor','Day-to-day operations'),
  ('fkhan','fact_checker','Verification across desks'),
  ('lpetrov','copy_editor','Copy & standards across desks'),
  ('mreyes','standards_legal','Standards & legal review'),
  ('cdubois','audience_editor','Audience & analytics')
) AS v(handle, role_key, reason)
JOIN meridian.users u ON u.handle = v.handle
CROSS JOIN (SELECT id FROM meridian.users WHERE handle = 'admin') a;

INSERT INTO meridian.role_assignments (id, user_id, role_key, scope_type, scope_id, starts_at, granted_by_id, reason)
SELECT gen_random_uuid(), u.id, 'section_editor', 'desk', d.id, now(), a.id, 'Section editor'
FROM (VALUES
  ('lchen','politics'),('rmalik','business'),('srossi','technology'),('tandersen','world'),
  ('nabara','science'),('jkim','culture'),('mflores','sports'),('hbennett','investigations')
) AS v(handle, desk_slug)
JOIN meridian.users u ON u.handle = v.handle
JOIN meridian.desks d ON d.slug = v.desk_slug
CROSS JOIN (SELECT id FROM meridian.users WHERE handle = 'admin') a;

-- 6. Stories — one row per (desk, workflow stage). Bodies use the app's block shape.
INSERT INTO meridian.stories
  (id, slug, title, dek, body, category, workflow_state, publication_state, priority,
   desk_id, author_id, editor_id, fact_checker_id, due_at, published_at, created_at, updated_at)
SELECT gen_random_uuid(), v.slug, v.title, v.dek,
       jsonb_build_array(
         jsonb_build_object('type','heading','text', v.title),
         jsonb_build_object('type','paragraph','text', v.dek || ' ' || 'This is seeded editorial copy for a realistic newsroom walkthrough.')
       ),
       v.category, v.wf, v.pub, v.pri,
       d.id, au.id, ed.id, fc.id,
       CASE WHEN v.due_days IS NULL THEN NULL ELSE now() + make_interval(days => v.due_days) END,
       CASE WHEN v.pub = 'published' THEN now() - make_interval(hours => v.age_h) ELSE NULL END,
       now() - make_interval(hours => v.age_h), now() - make_interval(hours => (v.age_h/2))
FROM (VALUES
  -- slug, title, dek, category, workflow_state, publication_state, priority, desk, author, editor, fact_checker, due_days, age_h
  ('budget-2026-winners-losers','Budget 2026: The Winners and Losers','A line-by-line look at who gains and who pays under the new fiscal plan.','Politics','ready','published','high','politics','aoconnor','lchen','fkhan',NULL,30),
  ('senate-hearing-live','Inside the Senate Hearing That Rattled the Cabinet','The vote was closer than the press release suggested.','Politics','desk_review','not_live','high','politics','aoconnor','lchen',NULL,2,26),
  ('coalition-talks','Coalition Talks Enter a Third Sleepless Night','Negotiators are haggling over three ministries.','Politics','drafting','not_live','medium','politics','aoconnor','lchen',NULL,NULL,10),
  ('central-bank-hold','Central Bank Holds Rates as Inflation Cools','Policymakers signalled patience despite market pressure.','Business','ready','published','high','business','bwright','rmalik','fkhan',NULL,52),
  ('chipmaker-earnings','Chipmaker''s Record Quarter Masks a Slowing Core','Six weeks of records, one clear pattern.','Business','verification','not_live','high','business','bwright','rmalik','fkhan',1,18),
  ('housing-permits','The Housing-Permit Backlog: What the Numbers Reveal','A closer look at the decisions, the money and the people in between.','Business','copy_standards','not_live','medium','business','bwright','rmalik',NULL,3,14),
  ('ai-safety-bill','New AI-Safety Bill Would Force Model Audits','The draft law lands on developers within a year.','Technology','ready','published','high','technology','ytanaka','srossi','fkhan',NULL,20),
  ('data-breach-probe','Data Breach at a Payments Giant Exposes Millions','What supporters call overdue, critics call a liability.','Technology','reporting','not_live','urgent','technology','ytanaka','srossi',NULL,1,8),
  ('platform-layoffs','Platform Layoffs Ripple Through the Contractor Economy','The cuts reach further than the headcount suggests.','Technology','assigned','not_live','medium','technology','ytanaka','srossi',NULL,5,6),
  ('frontline-dispatch','Dispatch From a Front Line That Keeps Moving','Long shadows lengthen across a contested plain.','World','ready','published','high','world','imueller','tandersen','fkhan',NULL,40),
  ('diplomacy-summit','A Summit Long on Handshakes, Short on Guarantees','Officials promised transparency; the door stayed shut.','World','desk_review','not_live','high','world','imueller','tandersen',NULL,2,22),
  ('refugee-corridor','The Corridor Where Aid and Politics Collide','Every crossing is a negotiation.','World','drafting','not_live','medium','world','imueller','tandersen',NULL,7,12),
  ('vaccine-rollout','Vaccine Rollout Reaches the Last Mile','The hardest doses are the final ten percent.','Science & Health','ready','published','medium','science','pnguyen','nabara','fkhan',NULL,64),
  ('climate-tipping','Scientists Warn a Climate Threshold Is Closer Than Thought','New models compress the timeline.','Science & Health','verification','not_live','high','science','pnguyen','nabara','fkhan',2,16),
  ('hospital-staffing','The Hospital Staffing Dispute Clears Committee as Costs Climb','What supporters call overdue, critics call a bill the public pays twice.','Science & Health','intake','not_live','low','science','pnguyen','nabara',NULL,14,4),
  ('festival-review','The Festival That Reinvented Itself Overnight','A programme built on risk finds its audience.','Culture','ready','published','low','culture','jkim','jkim',NULL,NULL,72),
  ('biennale-preview','A Biennale Preview That Doubles as a Manifesto','Curators turn the floor into an argument.','Culture','drafting','not_live','low','culture','jkim','jkim',NULL,9,10),
  ('cup-final-tactics','The Tactical Shift That Decided the Cup Final','One substitution, and the game turned.','Sports','ready','published','medium','sports','gsantos','mflores',NULL,NULL,28),
  ('transfer-window','Inside the Transfer Window''s Most Expensive Gamble','Clubs bet big on a striker with a fragile record.','Sports','copy_standards','not_live','medium','sports','gsantos','mflores',NULL,3,15),
  ('athletics-doping','Doping Case Reopens as New Samples Surface','A retest changes what the record book says.','Sports','reporting','not_live','high','sports','gsantos','mflores',NULL,4,7),
  ('offshore-accounts','The Offshore Accounts a Minister Forgot to Declare','Six months of records, one clear pattern.','Investigations','verification','not_live','urgent','investigations','aoconnor','hbennett','fkhan',5,24),
  ('procurement-trail','Following the Procurement Trail to a Shell Company','The paperwork points one way; the money another.','Investigations','desk_review','not_live','high','investigations','aoconnor','hbennett',NULL,6,20),
  ('whistleblower','A Whistleblower, a Warehouse and a Missing Ledger','What the audit was designed not to find.','Investigations','proposed','not_live','high','investigations','aoconnor','hbennett',NULL,12,3)
) AS v(slug,title,dek,category,wf,pub,pri,desk_slug,author_handle,editor_handle,fc_handle,due_days,age_h)
JOIN meridian.desks d  ON d.slug = v.desk_slug
JOIN meridian.users au ON au.handle = v.author_handle
JOIN meridian.users ed ON ed.handle = v.editor_handle
LEFT JOIN meridian.users fc ON fc.handle = v.fc_handle;

-- 7. Front page — feature a few published stories.
INSERT INTO meridian.front_page_slots (id, position, label, story_id, is_pinned, updated_by_id, updated_at)
SELECT gen_random_uuid(), v.position, v.label, s.id, v.pinned, a.id, now()
FROM (VALUES
  (1,'Lead', 'budget-2026-winners-losers', true),
  (2,'Top story','frontline-dispatch', false),
  (3,'Also featured','central-bank-hold', false),
  (4,'Also featured','ai-safety-bill', false)
) AS v(position, label, slug, pinned)
JOIN meridian.stories s ON s.slug = v.slug
CROSS JOIN (SELECT id FROM meridian.users WHERE handle='admin') a;

-- 8. Pitches — the commissioning queue in mixed states.
INSERT INTO meridian.pitches (id, headline, summary, angle, desk_id, created_by_id, assignee_id, editor_id, status, priority, key_questions, likely_sources, risks, created_at, updated_at)
SELECT gen_random_uuid(), v.headline, v.summary, v.angle, d.id, cr.id, asg.id, ed.id, v.status, v.priority,
       '[]'::jsonb, '[]'::jsonb, '[]'::jsonb, now() - make_interval(hours => v.age_h), now()
FROM (VALUES
  ('Who really owns the new stadium?','Trace the ownership behind the publicly funded arena.','Follow the money','investigations','aoconnor','aoconnor','hbennett','commissioned','high',30),
  ('The quiet return of the four-day week','Which firms kept it, and what changed.','Data-led feature','business','bwright','bwright','rmalik','commissioned','medium',26),
  ('AI in the classroom, one term in','What teachers actually think after a full term.','On-the-ground reporting','technology','ytanaka',NULL,'srossi','proposed','medium',10),
  ('The vaccine hesitancy map','Where uptake stalled and why.','Interactive','science','pnguyen',NULL,'nabara','needs_detail','medium',8),
  ('Inside the coalition''s war room','A fly-on-the-wall of the talks.','Access piece','politics','aoconnor',NULL,'lchen','proposed','high',6),
  ('The striker nobody wanted','A redemption arc built on one gamble.','Profile','sports','gsantos','gsantos','mflores','commissioned','low',18),
  ('A city rewilds its river','Culture meets climate on the waterfront.','Photo essay','culture','jkim',NULL,'jkim','parked','low',48),
  ('The border town that runs on aid','Economics of a permanent emergency.','Dispatch','world','imueller','imueller','tandersen','commissioned','high',12)
) AS v(headline, summary, angle, desk_slug, creator, assignee, editor, status, priority, age_h)
JOIN meridian.desks d ON d.slug = v.desk_slug
JOIN meridian.users cr ON cr.handle = v.creator
LEFT JOIN meridian.users asg ON asg.handle = v.assignee
JOIN meridian.users ed ON ed.handle = v.editor;

-- 9. Tasks — the sector task boards.
INSERT INTO meridian.tasks (id, story_id, desk_id, title, description, status, priority, assigned_to_id, created_by_id, due_at, created_at, updated_at)
SELECT gen_random_uuid(), s.id, d.id, v.title, v.description, v.status, v.priority, asg.id, cr.id,
       now() + make_interval(days => v.due_days), now() - make_interval(hours => v.age_h), now()
FROM (VALUES
  ('senate-hearing-live','politics','Confirm the vote count with two sources','Need on-record confirmation before desk review.','in_progress','high','aoconnor','lchen',1,20),
  ('chipmaker-earnings','business','Pull the segment revenue tables','Finance sent the deck; extract the core-vs-new split.','todo','medium','bwright','rmalik',2,14),
  ('data-breach-probe','technology','Get the company statement on record','They have until 5pm; escalate if silent.','blocked','urgent','ytanaka','srossi',1,7),
  ('climate-tipping','science','Second scientist to review the model claims','Independent read before we publish the timeline.','in_review','high','pnguyen','nabara',2,15),
  ('offshore-accounts','investigations','Verify the shell-company registration date','Companies-house record vs the declaration.','in_progress','urgent','aoconnor','hbennett',3,22),
  ('transfer-window','sports','Fact-check the fee against the release','Two outlets disagree on the number.','todo','medium','gsantos','mflores',2,12),
  ('frontline-dispatch','world','Clear the sensitive locations with legal','Standards to review before republication.','done','high','imueller','tandersen',-1,40),
  ('housing-permits','business','Rebuild the backlog chart from source data','Current chart uses stale figures.','todo','low','bwright','rmalik',4,10)
) AS v(story_slug, desk_slug, title, description, status, priority, assignee, creator, due_days, age_h)
JOIN meridian.stories s ON s.slug = v.story_slug
JOIN meridian.desks d ON d.slug = v.desk_slug
JOIN meridian.users asg ON asg.handle = v.assignee
JOIN meridian.users cr ON cr.handle = v.creator;

-- 10. Coverage events — the planning calendar.
INSERT INTO meridian.coverage_events (id, title, description, desk_id, owner_id, starts_at, ends_at, status, location, metadata_json, created_at, updated_at)
SELECT gen_random_uuid(), v.title, v.description, d.id, o.id,
       now() + make_interval(days => v.start_days, hours => 9),
       now() + make_interval(days => v.start_days, hours => 12),
       v.status, v.location, '{}'::jsonb, now(), now()
FROM (VALUES
  ('Budget statement to Parliament','Chancellor delivers the autumn statement.','politics','aoconnor',1,'Parliament','confirmed'),
  ('Central bank rate decision','Rate call plus press conference.','business','bwright',2,'Frankfurt','confirmed'),
  ('Product launch keynote','Flagship device and platform updates.','technology','ytanaka',3,'San Francisco','planned'),
  ('Peace-talks round three','Delegations resume negotiations.','world','imueller',4,'Geneva','tentative'),
  ('Climate assessment release','New IPCC-style regional report.','science','pnguyen',6,'Nairobi','planned'),
  ('Cup final','The season decider.','sports','gsantos',5,'Madrid','confirmed')
) AS v(title, description, desk_slug, owner_handle, start_days, location, status)
JOIN meridian.desks d ON d.slug = v.desk_slug
JOIN meridian.users o ON o.handle = v.owner_handle;

COMMIT;

-- Summary
SELECT 'users' AS entity, count(*) FROM meridian.users
UNION ALL SELECT 'desks', count(*) FROM meridian.desks
UNION ALL SELECT 'desk_memberships', count(*) FROM meridian.desk_memberships
UNION ALL SELECT 'role_assignments', count(*) FROM meridian.role_assignments
UNION ALL SELECT 'stories', count(*) FROM meridian.stories
UNION ALL SELECT 'stories_published', count(*) FROM meridian.stories WHERE publication_state='published'
UNION ALL SELECT 'pitches', count(*) FROM meridian.pitches
UNION ALL SELECT 'tasks', count(*) FROM meridian.tasks
UNION ALL SELECT 'coverage_events', count(*) FROM meridian.coverage_events
UNION ALL SELECT 'front_page_slots', count(*) FROM meridian.front_page_slots
ORDER BY entity;
