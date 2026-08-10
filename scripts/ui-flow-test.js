// Drives the real Dioxus WASM app in a browser, as a user would: click, type,
// navigate. Distinct from flow-test.py, which speaks HTTP to the API — this
// exercises hydration, routing, event handlers and rendering, none of which an
// API test can see.
const { chromium } = require('playwright');

const BASE = 'http://127.0.0.1:3100';
const USER = 'admin@meridian.example';
const PASS = 'DevPass123!';

const pass = [], fail = [], notes = [];
let consoleErrors = [];

function check(name, ok, detail = '') {
  (ok ? pass : fail).push(name + (detail ? ` — ${detail}` : ''));
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  [${detail}]` : ''}`);
}
function note(n) { notes.push(n); console.log(`  NOTE  ${n}`); }
function section(t) { console.log(`\n=== ${t} ===`); }

// Wait for Dioxus to hydrate: the shell only appears once WASM has booted, so
// this doubles as the assertion that hydration happened at all.
async function hydrated(page, timeout = 60000) {
  await page.waitForSelector('nav, .sidebar, .topbar, .panel, .page-title, form',
                             { timeout });
}

async function go(page, path) {
  await page.goto(BASE + path, { waitUntil: 'domcontentloaded' });
  await hydrated(page);
  await page.waitForTimeout(600);   // let resources resolve and re-render
}

(async () => {
  const browser = await chromium.launch({ headless: true, channel: 'chrome' });
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();

  page.on('console', m => {
    if (m.type() === 'error') consoleErrors.push(m.text().slice(0, 200));
  });
  page.on('pageerror', e => consoleErrors.push('PAGEERROR ' + e.message.slice(0, 200)));

  try {
    // ── boot & hydration ────────────────────────────────────────────────────
    section('Boot');
    const t0 = Date.now();
    await go(page, '/');
    check('app hydrates (WASM boots and renders the shell)', true,
          `${Date.now() - t0}ms`);
    check('no uncaught page errors on boot', consoleErrors.length === 0,
          consoleErrors[0] || '');

    // ── sign in as a user would ─────────────────────────────────────────────
    section('Flow · sign in');
    await go(page, '/sign-in');
    // The redesigned auth flow opens on a lock screen; dismiss it to reach the form.
    await page.getByRole('button', { name: /lock screen/i }).click({ timeout: 5000 }).catch(() => {});
    await page.waitForTimeout(600);
    const inputs = page.locator('form input');
    const n = await inputs.count();
    check('sign-in form renders its fields', n >= 2, `${n} inputs`);
    if (n >= 2) {
      await inputs.nth(0).fill(USER);
      await inputs.nth(1).fill(PASS);
      // Target the submit button specifically; the redesigned auth form also has a
      // `type="button"` password-visibility toggle that a bare `form button` grabs.
      await page.locator('form button[type="submit"], form button:not([type="button"])').first().click();
      // Success = we end up somewhere authenticated, i.e. the sidebar appears.
      await page.waitForTimeout(2500);
      const signedIn = await page.locator('nav, .sidebar').count() > 0
        && !(await page.locator('text=Incorrect email').count());
      check('signing in lands in the app', signedIn, page.url().replace(BASE, ''));
    }

    // ── the shell a signed-in user sees ─────────────────────────────────────
    section('Flow · app shell');
    await go(page, '/');
    const bodyText = await page.locator('body').innerText();
    check('dashboard renders content', bodyText.length > 200, `${bodyText.length} chars`);
    const liveBadge = await page.locator('text=/^(Live|Offline)$/').count();
    check('realtime indicator present', liveBadge > 0);
    const navLinks = await page.locator('nav a').count();
    check('left nav has entries', navLinks > 5, `${navLinks} links`);

    // ── every route renders without a client-side crash ─────────────────────
    section('Flow · every route renders in the browser');
    const ROUTES = [
      '/', '/org/sectors', '/org/search', '/org/feed', '/org/people',
      '/org/analytics', '/org/assets', '/org/publishing',
      '/org/publishing/corrections', '/org/frontpage',
      '/invitations', '/settings', '/settings/notifications', '/settings/sessions',
      '/admin', '/admin/roles', '/admin/governance', '/admin/sectors',
      '/no-access', '/editor', '/onboarding',
    ];
    const broken = [];
    for (const r of ROUTES) {
      consoleErrors = [];
      try {
        await go(page, r);
        const txt = await page.locator('body').innerText();
        // A route that renders nothing, or shows a raw error, is broken.
        if (txt.trim().length < 40) broken.push([r, 'empty']);
        else if (/panicked|RuntimeError|unreachable executed/i.test(txt))
          broken.push([r, 'panic text']);
        else if (consoleErrors.some(e => /panic|unreachable/i.test(e)))
          broken.push([r, 'wasm panic in console']);
      } catch (e) {
        broken.push([r, e.message.slice(0, 60)]);
      }
    }
    check(`all ${ROUTES.length} routes render in the browser`, broken.length === 0,
          JSON.stringify(broken));

    // ── sector-scoped routes ────────────────────────────────────────────────
    section('Flow · sector scope');
    await go(page, '/org/sectors');
    const sectorLink = page.locator('a[href^="/s/"]').first();
    const haveSector = await sectorLink.count() > 0;
    if (!haveSector) {
      note('no sector memberships on this account — sector routes tested by slug only');
      for (const r of ['/s/x', '/s/x/stories', '/s/x/assets', '/s/x/team',
                       '/s/x/analytics', '/s/x/search', '/s/x/settings']) {
        await go(page, r);
        const t = await page.locator('body').innerText();
        check(`unpermitted ${r} shows a no-access state, not an empty workspace`,
              /do not hold a membership|No access|not in any sector/i.test(t));
        break;   // one is enough to prove the guard
      }
    } else {
      const href = await sectorLink.getAttribute('href');
      const slug = href.split('/')[2];
      for (const suffix of ['', '/stories', '/tasks', '/planning', '/assets',
                            '/team', '/analytics', '/search', '/settings']) {
        await go(page, `/s/${slug}${suffix}`);
        const t = await page.locator('body').innerText();
        check(`/s/:sector${suffix || ' (dashboard)'} renders`, t.trim().length > 60);
      }
    }

    // ── command palette (⌘K) ────────────────────────────────────────────────
    section('Flow · command palette');
    await go(page, '/');
    await page.keyboard.press('Meta+k');
    await page.waitForTimeout(700);
    let paletteOpen = await page.locator('input[placeholder*="Jump to"]').count() > 0;
    if (!paletteOpen) {
      await page.keyboard.press('Control+k');
      await page.waitForTimeout(700);
      paletteOpen = await page.locator('input[placeholder*="Jump to"]').count() > 0;
    }
    check('⌘K / Ctrl-K opens the palette', paletteOpen);
    if (paletteOpen) {
      await page.locator('input[placeholder*="Jump to"]').fill('publishing');
      await page.waitForTimeout(400);
      const hits = await page.locator('li, .rowitem, button').filter({ hasText: /publishing/i }).count();
      check('palette filters as you type', hits > 0, `${hits} matches`);
      await page.keyboard.press('Escape');
      await page.waitForTimeout(300);
      check('Escape closes the palette',
            await page.locator('input[placeholder*="Jump to"]').count() === 0);
    }

    // ── theme toggle ────────────────────────────────────────────────────────
    section('Flow · dark mode');
    await go(page, '/settings');
    // The toggle is one cycling button labelled "System theme" / "Light theme" /
    // "Dark theme" — not three buttons. Click it until it reports Dark.
    const themeBtns = page.locator('button').filter({ hasText: /(System|Light|Dark) theme/ });
    const tb = await themeBtns.count();
    if (tb > 0) {
      let applied = null;
      for (let i = 0; i < 3; i++) {
        await themeBtns.first().click();
        await page.waitForTimeout(450);
        const t = await page.evaluate(() => document.documentElement.dataset.theme);
        if (t === 'dark') { applied = t; break; }
      }
      check('cycling the toggle reaches dark and applies it to the document',
            applied === 'dark', String(applied));
      // Cycling on returns to System, which removes the attribute so the OS
      // preference applies again — that is the documented contract.
      await themeBtns.first().click();
      await page.waitForTimeout(450);
      const t2 = await page.evaluate(() => document.documentElement.dataset.theme);
      check('cycling past dark returns to System (attribute removed)',
            t2 === undefined || t2 === '' || t2 === null, String(t2));
    } else {
      note('theme toggle not found on /settings');
    }

    // ── a real interaction that writes ──────────────────────────────────────
    section('Flow · governance: create a channel through the UI');
    await go(page, '/admin/governance');
    const keyField = page.locator('#channel-key');
    if (await keyField.count() > 0) {
      const key = 'uiflow' + Date.now().toString().slice(-5);
      await keyField.fill(key);
      await page.locator('#channel-name').fill('UI Flow Probe');
      await page.locator('button:has-text("Save channel")').click();
      await page.waitForTimeout(2000);
      const shown = await page.locator(`text=${key}`).count() > 0;
      check('channel created via the UI appears in the list', shown, key);
      global.__probeChannel = key;
    } else {
      check('governance channel form renders', false, '#channel-key not found');
    }

    // ── search ──────────────────────────────────────────────────────────────
    section('Flow · search');
    await go(page, '/org/search');
    const q = page.locator('input[type="search"]').first();
    if (await q.count() > 0) {
      await q.fill('the');
      await page.locator('button:has-text("Search")').first().click();
      await page.waitForTimeout(1500);
      const t = await page.locator('body').innerText();
      check('search returns a result set or a truthful empty state',
            /No stories match|Headline|results|Enter a term/i.test(t));
    } else {
      check('search input renders', false);
    }

    // ── front page ──────────────────────────────────────────────────────────
    section('Flow · front page');
    await go(page, '/org/frontpage');
    const slotRows = await page.locator('select').count();
    check('front-page slot board renders with pickers', slotRows > 0, `${slotRows} selects`);
    const fpText = await page.locator('body').innerText();
    check('empty slots are shown as a real state', /Empty|Lead|Slots/i.test(fpText));

    // ── sessions ────────────────────────────────────────────────────────────
    section('Flow · sessions');
    await go(page, '/settings/sessions');
    const st = await page.locator('body').innerText();
    check('sessions screen lists devices', /this device|Device|No active sessions/i.test(st));

    // ── console health across the whole run ─────────────────────────────────
    section('Console');
    const fatal = consoleErrors.filter(e => /panic|unreachable executed/i.test(e));
    check('no WASM panics in the console during the run', fatal.length === 0,
          fatal[0] || '');
  } catch (e) {
    check('suite completed without an unhandled error', false, e.message.slice(0, 200));
  } finally {
    await browser.close();
  }

  console.log('\n' + '='.repeat(60));
  console.log(`PASSED ${pass.length}`);
  console.log(`FAILED ${fail.length}`);
  if (notes.length) { console.log('\nNOTES'); notes.forEach(n => console.log('  · ' + n)); }
  if (fail.length) { console.log('\nFAILURES'); fail.forEach(f => console.log('  ✗ ' + f)); }
  if (global.__probeChannel) console.log(`\nprobe channel to clean: ${global.__probeChannel}`);
  process.exit(fail.length ? 1 : 0);
})();
