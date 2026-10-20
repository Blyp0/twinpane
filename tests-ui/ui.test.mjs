// End-to-end test for the Twinpane UI.
//
//   1. Make a practice folder (see README) and start the preview server on it:
//        cargo run -p twinpane-dev -- --root /tmp/tp-sandbox --port 8731
//   2. SANDBOX=/tmp/tp-sandbox CHROME=/path/to/chrome node ui.test.mjs
//
// The test uses real key presses and clicks, then checks the real files.

import fs from 'node:fs';
import path from 'node:path';
import puppeteer from 'puppeteer-core';

const SB = process.env.SANDBOX || '/tmp/tp-sandbox';
const URL_ = process.env.URL || 'http://127.0.0.1:8731/';
const CHROME = process.env.CHROME || '/opt/pw-browsers/chromium-1194/chrome-linux/chrome';
const SHOTS = process.env.SHOTS || '/tmp/shots';
fs.mkdirSync(SHOTS, { recursive: true });

const results = [];
const problems = [];
async function step(name, fn) {
  try { await fn(); results.push([name, true]); console.log('  ok   ', name); }
  catch (e) {
    results.push([name, false]); console.log('  FAIL ', name, '\n        ', e.message.split('\n')[0]);
    for (let i = 0; i < 4 && await page.$('dialog[open]'); i++) { await page.keyboard.press('Escape'); await sleep(120); }
  }
}
const assert = (c, m) => { if (!c) throw new Error(m || 'assertion failed'); };
const eq = (a, b, m = '') => assert(JSON.stringify(a) === JSON.stringify(b), `${m} expected ${JSON.stringify(b)}, got ${JSON.stringify(a)}`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const exists = (...p) => fs.existsSync(path.join(SB, ...p));

const browser = await puppeteer.launch({
  executablePath: CHROME, headless: true,
  args: ['--no-sandbox', '--disable-gpu', '--window-size=1440,900'],
});
const page = await browser.newPage();
await page.setViewport({ width: 1440, height: 900 });
page.on('response', (r) => { if (r.status() >= 400) problems.push(`status of ${r.status()}: ${r.request().method()} ${r.url().replace(URL_, '/')} ${(r.request().postData() || '').slice(0, 90)}`); });
page.on('pageerror', (e) => problems.push('pageerror: ' + e.message));
page.on('console', (m) => { if (m.type() === 'error' && !m.text().startsWith('Failed to load resource')) problems.push('console: ' + m.text()); });

const shot = (n) => page.screenshot({ path: path.join(SHOTS, n + '.png') });
const pane = (i) => `.pane[data-pane="${i}"]`;
const names = (i) => page.$$eval(`${pane(i)} .row .nm`, (n) => n.map((x) => x.textContent));
const pathOf = (i) => page.$eval(`${pane(i)} .path`, (x) => x.value);
// Poll from the test side (the app's CSP forbids eval inside the page, which is what we want).
async function waitRows(i, pred, t = 6000) {
  const end = Date.now() + t;
  let last = [];
  while (Date.now() < end) {
    last = await names(i);
    if (pred(last)) return last;
    await sleep(50);
  }
  throw new Error(`rows never matched ${pred.toString().slice(0, 70)}; last: ${last.slice(0, 6).join(', ')}`);
}
async function rowHandle(i, name) {
  const h = await page.evaluateHandle((sel, n) => [...document.querySelectorAll(sel + ' .row')].find((r) => r.querySelector('.nm').textContent === n), pane(i), name);
  assert(h.asElement(), `row ${name} not found in pane ${i}`);
  return h.asElement();
}
async function clickRow(i, name, { shift = false, ctrlKey = false } = {}) {
  const h = await rowHandle(i, name);
  if (shift) await page.keyboard.down('Shift');
  if (ctrlKey) await page.keyboard.down('Control');
  await h.click();
  if (shift) await page.keyboard.up('Shift');
  if (ctrlKey) await page.keyboard.up('Control');
}
async function dblRow(i, name) { await (await rowHandle(i, name)).click({ clickCount: 2 }); }
async function dialogBtn(text) {
  const h = await page.evaluateHandle((t) => [...document.querySelectorAll('dialog[open] button')].find((b) => b.textContent.trim().startsWith(t) && !b.disabled), text);
  assert(h.asElement(), `dialog button "${text}" not found or disabled`);
  await h.asElement().click();
}
const dialogOpen = () => page.waitForSelector('dialog[open]', { timeout: 4000 });
const dialogClosed = () => page.waitForFunction(() => !document.querySelector('dialog[open]'), { timeout: 6000 });
async function goTo(i, p) {
  await page.click(`${pane(i)} .list`);
  await page.keyboard.down('Control'); await page.keyboard.press('l'); await page.keyboard.up('Control');
  await page.keyboard.type(p); await page.keyboard.press('Enter');
  await page.waitForFunction((sel, p2) => document.querySelector(sel + ' .path').value === p2, { timeout: 5000 }, pane(i), p);
  await sleep(150);
}
const ctrl = async (k, shift = false) => {
  await page.keyboard.down('Control'); if (shift) await page.keyboard.down('Shift');
  await page.keyboard.press(k);
  if (shift) await page.keyboard.up('Shift'); await page.keyboard.up('Control');
};

console.log('\nTwinpane UI end-to-end test\n');
await page.goto(URL_, { waitUntil: 'networkidle0' });

await step('starts with two panes listing the sandbox', async () => {
  await waitRows(0, (r) => r.length >= 5);
  eq(await names(0), ['Big', 'Documents', 'Photos', 'Sync-A', 'Sync-B'], 'pane 0');
  eq(await pathOf(0), SB); eq(await pathOf(1), SB);
  assert((await page.$eval('#notice', (n) => n.textContent)).includes('sandbox'), 'preview notice shown');
  await shot('01-start');
});

await step('double-click opens a folder; Backspace goes up and re-selects it', async () => {
  await dblRow(0, 'Photos');
  await waitRows(0, (r) => r.includes('IMG_0001.jpg'));
  eq(await pathOf(0), path.join(SB, 'Photos'));
  await page.keyboard.press('Backspace');
  await waitRows(0, (r) => r.includes('Photos'));
  const sel = await page.$eval(`${pane(0)} .row.sel .nm`, (n) => n.textContent);
  eq(sel, 'Photos', 'folder we came from is selected');
  await dblRow(0, 'Photos');
  await waitRows(0, (r) => r.includes('IMG_0001.jpg'));
});

await step('Date taken column shows EXIF dates and sorts, missing dates last', async () => {
  await page.click('.menu-wrap > button');
  await page.evaluate(() => { for (const l of document.querySelectorAll('.menu label')) if (['Date taken', 'Camera'].includes(l.textContent.trim())) l.querySelector('input').click(); });
  await page.waitForFunction((sel) => document.querySelector(sel + ' .head').textContent.includes('Date taken'), {}, pane(0));
  await sleep(300);
  await page.mouse.click(5, 400); // close menu
  const th = await page.evaluateHandle((sel) => [...document.querySelectorAll(sel + ' .th')].find((b) => b.textContent.startsWith('Date taken')), pane(0));
  await th.asElement().click();
  await waitRows(0, (r) => r[0] === 'IMG_0004.jpg');
  eq((await names(0)).slice(0, 3), ['IMG_0004.jpg', 'IMG_0002.jpg', 'IMG_0005.jpg'], 'ascending');
  eq((await names(0)).at(-1), 'no_exif.png', 'file without EXIF sorts last');
  const first = await page.$eval(`${pane(0)} .row`, (r) => r.textContent);
  assert(first.includes('2019-03-03 21:45'), 'date taken text: ' + first);
  await (await page.evaluateHandle((sel) => [...document.querySelectorAll(sel + ' .th')].find((b) => b.textContent.startsWith('Date taken')), pane(0))).asElement().click();
  await waitRows(0, (r) => r[0] === 'IMG_0003.jpg');
  eq((await names(0)).at(-1), 'no_exif.png', 'still last when descending');
  const cam = await page.$eval(`${pane(0)} .row`, (r) => r.textContent);
  assert(/Canon|NIKON|ILCE/i.test(cam), 'camera column: ' + cam);
  await shot('02-photos-metadata');
});

await step('Tab switches the active pane; arrows move the cursor; typeahead jumps', async () => {
  await page.click(`${pane(0)} .list`);
  await page.keyboard.press('Tab');
  assert(await page.$eval(pane(1), (p) => p.classList.contains('active')), 'pane 1 active');
  await page.keyboard.press('Tab');
  assert(await page.$eval(pane(0), (p) => p.classList.contains('active')), 'pane 0 active');
  await page.keyboard.press('Home');
  await page.keyboard.press('ArrowDown');
  await page.keyboard.type('no_');
  eq(await page.$eval(`${pane(0)} .row.cursor .nm`, (n) => n.textContent), 'no_exif.png', 'typeahead');
});

await step('path bar navigates (Ctrl+L) and rejects bad paths without breaking', async () => {
  await goTo(1, path.join(SB, 'Documents'));
  await waitRows(1, (r) => r.includes('notes.txt'));
  eq((await names(1)).sort(), ['Report.TXT', 'notes.txt'], 'hidden file not shown');
  await page.click(`${pane(1)} .list`);
  await ctrl('l'); await page.keyboard.type('/etc'); await page.keyboard.press('Enter');
  await page.waitForSelector('.toast.error', { timeout: 4000 });
  eq(await pathOf(1), path.join(SB, 'Documents'), 'path unchanged after refusal');
});

await step('F5 copies to the other pane; Ctrl+Z undoes it', async () => {
  await clickRow(0, 'IMG_0001.jpg');
  await page.keyboard.press('F5');
  await dialogOpen();
  await shot('03-copy-dialog');
  await dialogBtn('Copy');
  await dialogClosed();
  await waitRows(1, (r) => r.includes('IMG_0001.jpg'));
  assert(exists('Documents', 'IMG_0001.jpg'), 'file copied on disk');
  assert(exists('Photos', 'IMG_0001.jpg'), 'original kept');
  await page.click(`${pane(0)} .list`);
  await ctrl('z');
  await waitRows(1, (r) => !r.includes('IMG_0001.jpg'));
  assert(!exists('Documents', 'IMG_0001.jpg'), 'copy undone');
});

await step('copy conflict: "Keep both" names the copy "(2)"', async () => {
  await clickRow(0, 'IMG_0001.jpg'); await page.keyboard.press('F5'); await dialogOpen(); await dialogBtn('Copy'); await dialogClosed();
  await clickRow(0, 'IMG_0001.jpg'); await page.keyboard.press('F5'); await dialogOpen(); await dialogBtn('Copy'); await dialogClosed();
  await waitRows(1, (r) => r.includes('IMG_0001 (2).jpg'));
  assert(exists('Documents', 'IMG_0001 (2).jpg'), 'second copy renamed');
  await ctrl('z'); await ctrl('z');
  await waitRows(1, (r) => !r.includes('IMG_0001.jpg') && !r.includes('IMG_0001 (2).jpg'));
});

await step('F7 creates a folder, Delete removes it to holding, Undo brings it back', async () => {
  await page.click(`${pane(0)} .list`);
  await page.keyboard.press('F7'); await dialogOpen();
  await page.keyboard.type('Vacation'); await page.keyboard.press('Enter'); await dialogClosed();
  await waitRows(0, (r) => r.includes('Vacation'));
  assert(exists('Photos', 'Vacation'), 'folder created');
  eq(await page.$eval(`${pane(0)} .row.cursor .nm`, (n) => n.textContent), 'Vacation', 'new folder selected');
  await page.keyboard.press('Delete'); await dialogOpen();
  await shot('04-delete-confirm');
  await dialogBtn('Delete'); await dialogClosed();
  await waitRows(0, (r) => !r.includes('Vacation'));
  assert(!exists('Photos', 'Vacation'), 'folder gone');
  await ctrl('z');
  await waitRows(0, (r) => r.includes('Vacation'));
  assert(exists('Photos', 'Vacation'), 'delete undone');
  await ctrl('z'); // undo the mkdir
  await waitRows(0, (r) => !r.includes('Vacation'));
});

await step('F2 renames a single item and Undo reverts it', async () => {
  await clickRow(0, 'no_exif.png');
  await page.keyboard.press('F2'); await dialogOpen();
  eq(await page.$eval('dialog[open] input', (i) => i.value), 'no_exif.png');
  await page.keyboard.down('Control'); await page.keyboard.press('a'); await page.keyboard.up('Control');
  await page.keyboard.type('plain.png'); await page.keyboard.press('Enter'); await dialogClosed();
  await waitRows(0, (r) => r.includes('plain.png'));
  assert(exists('Photos', 'plain.png') && !exists('Photos', 'no_exif.png'), 'renamed on disk');
  await ctrl('z');
  await waitRows(0, (r) => r.includes('no_exif.png'));
});

await step('batch rename: live diff preview, conflicts block apply, apply is one undo step', async () => {
  // select the six JPEGs (sorted by date taken, descending: all but the last row)
  const rows = await names(0);
  await clickRow(0, rows[0]);
  await clickRow(0, rows[5], { shift: true });
  eq(await page.$$eval(`${pane(0)} .row.sel`, (r) => r.length), 6, 'six selected');
  await ctrl('r'); await dialogOpen();
  const fields = await page.$$('dialog[open] .rule input.field');
  await fields[0].type('^.*$');
  await (await page.$('dialog[open] .rule .chk input')).click(); // regular expression
  await (await page.$$('dialog[open] .rule input.field'))[1].type('{taken:%Y%m%d_%H%M%S}');
  await page.waitForFunction(() => document.querySelector('dialog[open] .rn-preview').textContent.includes('20230405_172700.jpg'), { timeout: 5000 });
  const preview = await page.$eval('dialog[open] .rn-preview', (n) => n.textContent);
  assert(preview.includes('20190303_214510.jpg') && preview.includes('20241224_081530.jpg'), 'new names from EXIF: ' + preview);
  assert(await page.$('dialog[open] .rn-row .add'), 'changed segment highlighted');
  assert(!(await page.$eval('dialog[open]', (d) => d.textContent)).includes('null'), 'no stray "null" text in the dialog');
  await shot('05-batch-rename-preview');
  const label = await page.$eval('dialog[open] .dlg-foot .btn.primary', (b) => b.textContent);
  eq(label, 'Rename 6 items');
  // an invalid regex is reported, not crashed on
  const f0 = (await page.$$('dialog[open] .rule input.field'))[0];
  await f0.click({ clickCount: 3 }); await f0.type('(');
  await page.waitForFunction(() => !document.querySelector('dialog[open] .err').hidden, { timeout: 4000 });
  assert(await page.$eval('dialog[open] .dlg-foot .btn.primary', (b) => b.disabled), 'apply disabled on bad regex');
  await f0.click({ clickCount: 3 }); await f0.type('^.*$');
  await page.waitForFunction(() => document.querySelector('dialog[open] .err').hidden && !document.querySelector('dialog[open] .dlg-foot .btn.primary').disabled, { timeout: 4000 });
  await dialogBtn('Rename 6');
  await dialogClosed();
  await waitRows(0, (r) => r.includes('20230405_172700.jpg'));
  assert(exists('Photos', '20230405_172700.jpg') && !exists('Photos', 'IMG_0001.jpg'), 'renamed on disk');
  await page.click(`${pane(0)} .list`);
  await ctrl('z');
  await waitRows(0, (r) => r.includes('IMG_0001.jpg') && !r.includes('20230405_172700.jpg'));
  for (let i = 1; i <= 6; i++) assert(exists('Photos', `IMG_000${i}.jpg`), `IMG_000${i}.jpg restored by ONE undo`);
});

await step('batch rename flags a file without EXIF and refuses to apply', async () => {
  await page.click(`${pane(0)} .list`);
  await ctrl('a');
  await ctrl('r'); await dialogOpen();
  await (await page.$$('dialog[open] .rule input.field'))[0].type('^.*$');
  await (await page.$('dialog[open] .rule .chk input')).click();
  await (await page.$$('dialog[open] .rule input.field'))[1].type('{taken:%Y}');
  await page.waitForFunction(() => [...document.querySelectorAll('dialog[open] .rn-preview .chip.bad')].some((c) => /No taken data/.test(c.textContent)), { timeout: 5000 });
  const chips = await page.$$eval('dialog[open] .rn-preview .chip.bad', (c) => c.map((x) => x.textContent));
  assert(chips.some((c) => /Duplicate/.test(c)), 'duplicate names are flagged too: ' + chips);
  assert(await page.$eval('dialog[open] .dlg-foot .btn.primary', (b) => b.disabled), 'apply disabled');
  await shot('06-batch-rename-conflict');
  await dialogBtn('Cancel'); await dialogClosed();
});

await step('Find: contents search opens a results tab with line matches', async () => {
  await goTo(0, SB);
  await ctrl('f'); await dialogOpen();
  const inputs = await page.$$('dialog[open] .find-form input[type=text]');
  await inputs[1].type('txt');                     // name contains "txt"
  await (await page.$$('dialog[open] .find-form input[type=text]'))[3].type('todo'); // contains text
  await shot('07-find-dialog');
  await page.keyboard.press('Enter');
  await dialogClosed();
  await page.waitForSelector(`${pane(0)} .tab.on .ic`, { timeout: 5000 });
  const tab = await page.$eval(`${pane(0)} .tab.on`, (t) => t.textContent);
  assert(tab.startsWith('Find:'), tab);
  await waitRows(0, (r) => r.length === 2);
  eq((await names(0)).sort(), ['Report.TXT', 'notes.txt']);
  const txt = await page.$eval(`${pane(0)} .list`, (l) => l.textContent);
  assert(txt.includes('1: todo: buy milk') && txt.includes('2: TODO: send'), 'match lines shown: ' + txt);
  await shot('08-find-results');
  // results are operable like any folder: open reveals the file in its folder in a new tab
  await dblRow(0, 'notes.txt');
  await page.waitForFunction((sel) => document.querySelector(sel + ' .path').value.endsWith('Documents'), {}, pane(0));
  await waitRows(0, (r) => r.includes('notes.txt'));
  eq(await page.$eval(`${pane(0)} .row.sel .nm`, (n) => n.textContent), 'notes.txt');
  eq((await page.$$(`${pane(0)} .tab`)).length, 3, 'original folder, results, and the revealed folder');
});

await step('tabs: Ctrl+W closes the current tab and keeps the right one', async () => {
  await page.click(`${pane(0)} .list`);
  await ctrl('w'); await sleep(250);
  eq((await page.$$(`${pane(0)} .tab`)).length, 2, 'two tabs left');
  eq(await page.evaluate(() => window.__twinpane.panes[0].ti), 1, 'moved to the neighbouring tab');
  assert((await page.$eval(`${pane(0)} .tab.on`, (t) => t.textContent)).startsWith('Find:'), 'results tab is current');
  await ctrl('w'); await sleep(250);
  eq((await page.$$(`${pane(0)} .tab`)).length, 1, 'one tab left');
  eq(await page.evaluate(() => window.__twinpane.panes[0].ti), 0);
  await ctrl('w'); await sleep(150);
  eq((await page.$$(`${pane(0)} .tab`)).length, 1, 'the last tab cannot be closed');
  await ctrl('t'); await sleep(300);
  eq((await page.$$(`${pane(0)} .tab`)).length, 2, 'new tab');
  await ctrl('w'); await sleep(200);
  eq((await page.$$(`${pane(0)} .tab`)).length, 1, 'closed again');
});

await step('Compare and sync (Mirror): preview, confirm removals, apply, one undo', async () => {
  await goTo(0, path.join(SB, 'Sync-A'));
  await goTo(1, path.join(SB, 'Sync-B'));
  await page.click(`${pane(0)} .list`);
  await ctrl('s', true); await dialogOpen();
  await page.waitForFunction(() => document.querySelector('dialog[open] .sync-table .srow'), { timeout: 5000 });
  await page.select('dialog[open] select[aria-label="Sync mode"]', 'Mirror');
  await page.waitForFunction(() => document.querySelector('dialog[open] .sync-table').textContent.includes('Remove from right'), { timeout: 5000 });
  const t = await page.$eval('dialog[open] .sync-table', (n) => n.textContent);
  assert(t.includes('Copy to right') && t.includes('only_left.txt') && t.includes('only_right.txt'), t);
  assert(exists('Sync-B', 'only_right.txt'), 'dry run changed nothing');
  assert(!(await page.$eval('dialog[open]', (d) => d.textContent)).includes('null'), 'no stray "null" text in the sync dialog');
  await shot('09-sync-preview');
  await dialogBtn('Run sync');
  await page.waitForFunction(() => document.querySelectorAll('dialog[open]').length === 2, { timeout: 4000 }); // confirm opens
  const dlgs = await page.$$('dialog[open]');
  const confirmBtn = await dlgs[1].evaluateHandle((d) => [...d.querySelectorAll('button')].find((b) => b.textContent === 'Run sync'));
  await confirmBtn.asElement().click();
  await dialogClosed();
  assert(exists('Sync-B', 'only_left.txt') && !exists('Sync-B', 'only_right.txt'), 'right now mirrors left');
  eq(fs.readFileSync(path.join(SB, 'Sync-B', 'sub', 'shared.txt'), 'utf8'), 'new version');
  await sleep(300);
  await ctrl('z');
  await sleep(500);
  assert(exists('Sync-B', 'only_right.txt') && !exists('Sync-B', 'only_left.txt'), 'one undo restores everything');
  eq(fs.readFileSync(path.join(SB, 'Sync-B', 'sub', 'shared.txt'), 'utf8'), 'old');
});

await step('layout: panes fill the window even when the preview notice is hidden (as in the desktop app)', async () => {
  await page.evaluate(() => { document.getElementById('notice').hidden = true; });
  await sleep(150);
  const m = await page.evaluate(() => ({
    win: innerHeight,
    panes: document.getElementById('panes').getBoundingClientRect().height,
    statusBottom: document.getElementById('status').getBoundingClientRect().bottom,
    tool: document.getElementById('toolbar').getBoundingClientRect().height,
  }));
  assert(m.panes > m.win - m.tool - 80, `panes should fill the window: panes ${m.panes}px of ${m.win}px`);
  assert(Math.abs(m.statusBottom - m.win) < 2, `status bar sits at the bottom edge (bottom ${m.statusBottom}, window ${m.win})`);
  await page.evaluate(() => { document.getElementById('notice').hidden = false; });
});

await step('virtual list: 3,000 files render only a window and End reaches the last', async () => {
  await goTo(0, path.join(SB, 'Big'));
  await waitRows(0, (r) => r.length > 10);
  const rendered = await page.$$eval(`${pane(0)} .row`, (r) => r.length);
  assert(rendered < 80, `only a window of rows in the DOM (got ${rendered})`);
  eq(await page.$eval(`${pane(0)} .spacer`, (s) => s.style.height), `${3000 * 26}px`);
  await page.click(`${pane(0)} .list`);
  await page.keyboard.press('End');
  await waitRows(0, (r) => r.includes('file_2999.dat'));
  assert((await page.$eval('#st-sel', (s) => s.textContent)).includes('1 of 3000'), 'status line');
  await page.keyboard.down('Control'); await page.keyboard.press('a'); await page.keyboard.up('Control');
  assert((await page.$eval('#st-sel', (s) => s.textContent)).includes('3000 of 3000'), 'select all counts every file');
  await shot('10-big-folder');
});

await step('the API refuses paths outside the sandbox even with a valid token', async () => {
  const out = await page.evaluate(async () => {
    const token = document.querySelector('meta[name="twinpane-token"]').content;
    const r = await fetch('/api/list_rows', { method: 'POST', headers: { 'content-type': 'application/json', 'x-twinpane-token': token }, body: JSON.stringify({ path: '/etc' }) });
    return { status: r.status, body: await r.json() };
  });
  eq(out.status, 400);
  assert(out.body.error.includes('sandbox'), JSON.stringify(out));
});

await step('dark mode renders (screenshot for review)', async () => {
  await page.emulateMediaFeatures([{ name: 'prefers-color-scheme', value: 'dark' }]);
  await goTo(0, path.join(SB, 'Photos'));
  await sleep(300);
  await shot('11-dark');
  await page.emulateMediaFeatures([{ name: 'prefers-color-scheme', value: 'light' }]);
});

await step('no JavaScript errors were logged during the whole run', async () => {
  // Three requests are refused on purpose: the path bar pointed at /etc, the direct API probe
  // outside the sandbox, and the deliberately invalid regex in the rename dialog. The browser
  // logs each refused request; anything else is a real problem.
  const expected = problems.filter((m) => m.includes('status of 400'));
  const unexpected = problems.filter((m) => !m.includes('status of 400'));
  assert(expected.length <= 3, `more refused requests than expected (${expected.length}): ${expected.join(' || ')}`);
  assert(unexpected.length === 0, unexpected.join(' | '));
});

await browser.close();
const failed = results.filter((r) => !r[1]);
console.log(`\n${results.length - failed.length}/${results.length} steps passed. Screenshots: ${SHOTS}\n`);
process.exit(failed.length ? 1 : 0);
