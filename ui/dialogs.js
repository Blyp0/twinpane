import { call } from './api.js';
import { el, icon, plural, formatSize, formatDate, debounce, toast, basename } from './util.js';

// ---------------------------------------------------------------------------
// Building blocks

function openDialog(d, { onOpen } = {}) {
  document.body.append(d);
  d.showModal();
  if (onOpen) onOpen();
  return new Promise((resolve) => {
    d.addEventListener('close', () => { d.remove(); resolve(d.returnValue); }, { once: true });
  });
}

function shell(title, cls = '') {
  const body = el('div', { class: 'dlg-body' });
  const foot = el('div', { class: 'dlg-foot' });
  const d = el('dialog', { class: `dlg ${cls}`, 'aria-label': title },
    el('header', { class: 'dlg-head' }, el('h2', {}, title)), body, foot);
  return { d, body, foot };
}

function btn(label, { kind = '', onclick, disabled = false, type = 'button', id, form } = {}) {
  return el('button', { class: `btn ${kind}`.trim(), type, onclick, disabled, id, form }, label);
}

const lines = (list, max, mapper) => {
  const shown = list.slice(0, max).map(mapper);
  if (list.length > max) shown.push(el('li', { class: 'muted' }, `and ${list.length - max} more`));
  return shown;
};

// ---------------------------------------------------------------------------
// Confirm / prompt / transfer / report

export function confirmDialog({ title, body, confirmLabel = 'OK', cancelLabel = 'Cancel', danger = false }) {
  const { d, body: b, foot } = shell(title, 'small');
  b.append(...[body].flat());
  const ok = btn(confirmLabel, { kind: danger ? 'danger' : 'primary', onclick: () => d.close('ok') });
  foot.append(btn(cancelLabel, { onclick: () => d.close('') }), ok);
  return openDialog(d, { onOpen: () => ok.focus() }).then((v) => v === 'ok');
}

export function promptDialog({ title, label, value = '', confirmLabel = 'OK', stem = false }) {
  const { d, body, foot } = shell(title, 'small');
  const input = el('input', { type: 'text', class: 'field', value, spellcheck: 'false', autocomplete: 'off' });
  const err = el('p', { class: 'err', hidden: true });
  const submit = () => {
    if (!input.value.trim()) { err.textContent = 'Enter a name.'; err.hidden = false; return; }
    d.close(input.value);
  };
  input.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); submit(); } });
  body.append(el('label', { class: 'lbl-col' }, label, input), err);
  foot.append(btn('Cancel', { onclick: () => d.close('') }), btn(confirmLabel, { kind: 'primary', onclick: submit }));
  return openDialog(d, {
    onOpen: () => {
      input.focus();
      const dot = value.lastIndexOf('.');
      if (stem && dot > 0) input.setSelectionRange(0, dot); else input.select();
    },
  }).then((v) => (v === '' ? null : v));
}

const POLICIES = [
  ['KeepBoth', 'Keep both', 'The new item gets a name like "photo (2).jpg".'],
  ['Skip', 'Skip existing', 'Anything already there is left alone.'],
  ['Overwrite', 'Replace existing', 'The old version is kept in the holding area, so Undo brings it back.'],
  ['Fail', 'Stop on conflict', 'Each item that already exists is reported as an error.'],
];

export function transferDialog({ kind, rows, dest }) {
  const verb = kind === 'copy' ? 'Copy' : 'Move';
  const { d, body, foot } = shell(`${verb} ${plural(rows.length, 'item')}`, 'small');
  const sel = el('select', { class: 'field' }, POLICIES.map(([v, l]) => el('option', { value: v }, l)));
  const help = el('p', { class: 'muted' });
  const upd = () => { help.textContent = POLICIES.find((p) => p[0] === sel.value)[2]; };
  sel.addEventListener('change', upd);
  upd();
  body.append(
    el('p', {}, `${verb} to`),
    el('p', { class: 'pathline' }, dest),
    el('label', { class: 'lbl-col' }, 'If an item already exists', sel),
    help,
  );
  const go = btn(verb, { kind: 'primary', onclick: () => d.close(sel.value) });
  foot.append(btn('Cancel', { onclick: () => d.close('') }), go);
  return openDialog(d, { onOpen: () => go.focus() }).then((v) => (v ? { policy: v } : null));
}

export function reportDialog({ title, skipped = [], errors = [] }) {
  const { d, body, foot } = shell(title, 'small');
  if (errors.length) {
    body.append(
      el('p', { class: 'err' }, `${plural(errors.length, 'item')} could not be processed:`),
      el('ul', { class: 'plain report' }, lines(errors, 30, ([p, m]) => el('li', {}, el('strong', {}, basename(p)), ` ${m}`))),
    );
  }
  if (skipped.length) {
    body.append(
      el('p', {}, `${plural(skipped.length, 'item')} skipped:`),
      el('ul', { class: 'plain report' }, lines(skipped, 30, ([p, m]) => el('li', {}, el('strong', {}, basename(p)), ` ${m}`))),
    );
  }
  const ok = btn('Close', { kind: 'primary', onclick: () => d.close('ok') });
  foot.append(ok);
  return openDialog(d, { onOpen: () => ok.focus() });
}

// ---------------------------------------------------------------------------
// Batch rename

const TARGETS = [['Stem', 'Name without extension'], ['Extension', 'Extension'], ['Name', 'Whole name']];
const CASES = [['Lower', 'lowercase'], ['Upper', 'UPPERCASE'], ['Title', 'Title Case'], ['Sentence', 'Sentence case']];
const RULE_LABEL = { Replace: 'Find and replace', Insert: 'Insert text', Case: 'Change case', Trim: 'Trim spaces' };

function newRule(type) {
  switch (type) {
    case 'Replace': return { type, find: '', with: '', regex: false, ignore_case: false, target: 'Stem' };
    case 'Insert': return { type, text: '', at: 'Start', index: 0, target: 'Stem' };
    case 'Case': return { type, mode: 'Lower', target: 'Name' };
    default: return { type: 'Trim', target: 'Stem' };
  }
}

function ruleJson(r) {
  switch (r.type) {
    case 'Replace':
      return { type: 'Replace', find: r.find, with: r.with, regex: r.regex, ignore_case: r.ignore_case, target: r.target };
    case 'Insert':
      return {
        type: 'Insert', text: r.text, target: r.target,
        at: r.at === 'Index' ? { Index: Math.max(0, parseInt(r.index, 10) || 0) } : r.at,
      };
    case 'Case': return { type: 'Case', mode: r.mode, target: r.target };
    default: return { type: 'Trim', target: r.target };
  }
}

const PRESETS = [
  ['Number files', () => [{ ...newRule('Replace'), find: '^.*$', with: '{stem}_{n:3}', regex: true }]],
  ['Prefix with date taken', () => [{ ...newRule('Insert'), text: '{taken:%Y-%m-%d}_' }]],
  ['Lowercase', () => [newRule('Case')]],
  ['Spaces to underscores', () => [newRule('Trim'), { ...newRule('Replace'), find: ' ', with: '_' }]],
];

function statusInfo(s) {
  if (typeof s === 'string') return { kind: s, msg: '' };
  const [k, v] = Object.entries(s)[0];
  return { kind: k, msg: v };
}

function diffParts(a, b) {
  const max = Math.min(a.length, b.length);
  let p = 0;
  while (p < max && a[p] === b[p]) p++;
  let s = 0;
  while (s < max - p && a[a.length - 1 - s] === b[b.length - 1 - s]) s++;
  return { pre: a.slice(0, p), oldMid: a.slice(p, a.length - s), newMid: b.slice(p, b.length - s), post: a.slice(a.length - s) };
}

export function renameDialog({ paths }) {
  const { d, body, foot } = shell(`Batch rename ${plural(paths.length, 'item')}`, 'wide rename');
  const rules = [newRule('Replace')];
  const opts = { counter_start: 1, counter_step: 1 };
  let seq = 0;
  let latest = null;

  const rulesEl = el('div', { class: 'rn-rules' });
  const previewEl = el('div', { class: 'rn-preview', role: 'table', 'aria-label': 'Rename preview' });
  const banner = el('p', { class: 'err', hidden: true, role: 'alert' });
  const summary = el('span', { class: 'summary', role: 'status' });
  const apply = btn('Rename', { kind: 'primary', disabled: true });

  const payload = () => ({
    paths,
    rules: rules.map(ruleJson),
    options: {
      counter_start: parseInt(opts.counter_start, 10) || 0,
      counter_step: parseInt(opts.counter_step, 10) || 1,
      utc_offset_minutes: -new Date().getTimezoneOffset(),
    },
  });

  const schedule = debounce(() => preview(), 150);

  async function preview() {
    const my = ++seq;
    try {
      const res = await call('rename_preview', payload());
      if (my !== seq) return;
      latest = res;
      banner.hidden = true;
      renderPreview(res);
    } catch (e) {
      if (my !== seq) return;
      banner.textContent = e.message;
      banner.hidden = false;
      apply.disabled = true;
      previewEl.classList.add('stale');
    }
  }

  function renderPreview(res) {
    previewEl.classList.remove('stale');
    const MAX = 400;
    const head = el('div', { class: 'rn-row rn-head', role: 'row' },
      el('span', {}, 'Current name'), el('span', {}, 'New name'), el('span', {}, ''));
    const rows = res.items.slice(0, MAX).map((it) => {
      const st = statusInfo(it.status);
      const bad = !['Ok', 'Unchanged'].includes(st.kind);
      const { pre, oldMid, newMid, post } = diffParts(it.old_name, it.new_name);
      const same = it.old_name === it.new_name;
      const chip = {
        Unchanged: ['unchanged', 'muted'],
        Invalid: ['Invalid name', 'bad'],
        Duplicate: ['Duplicate name', 'bad'],
        Exists: ['Already exists', 'bad'],
        MissingMetadata: [`No ${st.msg} data`, 'bad'],
      }[st.kind];
      return el('div', { class: 'rn-row' + (bad ? ' bad' : ''), role: 'row' },
        el('span', { class: 'old', role: 'cell' }, same ? it.old_name : [pre, oldMid ? el('s', { class: 'del' }, oldMid) : null, post]),
        el('span', { class: 'new', role: 'cell' }, same ? it.new_name : [pre, newMid ? el('b', { class: 'add' }, newMid) : null, post]),
        el('span', { class: 'chip-cell', role: 'cell' }, chip ? el('span', { class: `chip ${chip[1]}`, title: st.msg }, chip[0]) : null));
    });
    previewEl.replaceChildren(head, ...rows,
      ...(res.items.length > MAX ? [el('p', { class: 'muted pad' }, `Showing the first ${MAX} of ${res.items.length}.`)] : []));
    const parts = [`${plural(res.changes, 'item')} will be renamed`];
    if (res.conflicts) parts.push(`${plural(res.conflicts, 'conflict')} to fix`);
    summary.textContent = parts.join(', ');
    summary.classList.toggle('bad', res.conflicts > 0);
    apply.disabled = !res.applicable;
    apply.textContent = res.changes ? `Rename ${plural(res.changes, 'item')}` : 'Rename';
  }

  // ---- rule editors
  const field = (label, input) => el('label', { class: 'fld' }, el('span', { class: 'fld-l' }, label), input);
  const text = (r, key, ph = '') => el('input', {
    type: 'text', class: 'field', value: r[key], placeholder: ph, spellcheck: 'false', autocomplete: 'off',
    oninput: (e) => { r[key] = e.target.value; schedule(); },
  });
  const check = (r, key, label) => el('label', { class: 'chk' },
    el('input', { type: 'checkbox', checked: r[key], onchange: (e) => { r[key] = e.target.checked; schedule(); } }), label);
  const select = (r, key, options, rerender = false) => el('select', {
    class: 'field',
    onchange: (e) => { r[key] = e.target.value; if (rerender) renderRules(); schedule(); },
  }, options.map(([v, l]) => el('option', { value: v, selected: r[key] === v }, l)));

  function ruleCard(r, i) {
    let fields;
    if (r.type === 'Replace') {
      fields = [
        field('Find', text(r, 'find', r.regex ? 'regular expression' : 'text to find')),
        field('Replace with', text(r, 'with', r.regex ? 'use $1 for a group, {n:3} for a counter' : '{n:3}, {taken:%Y%m%d}, …')),
        el('div', { class: 'inline' }, check(r, 'regex', 'Regular expression'), check(r, 'ignore_case', 'Ignore case')),
        field('Apply to', select(r, 'target', TARGETS)),
      ];
    } else if (r.type === 'Insert') {
      fields = [
        field('Text', text(r, 'text', '{n:3}, {taken:%Y-%m-%d}_, …')),
        el('div', { class: 'inline' },
          field('Where', select(r, 'at', [['Start', 'At the start'], ['End', 'At the end'], ['Index', 'After character…']], true)),
          r.at === 'Index' ? field('Character', el('input', {
            type: 'number', min: '0', class: 'field num', value: r.index,
            oninput: (e) => { r.index = e.target.value; schedule(); },
          })) : null),
        field('Apply to', select(r, 'target', TARGETS)),
      ];
    } else if (r.type === 'Case') {
      fields = [field('Change to', select(r, 'mode', CASES)), field('Apply to', select(r, 'target', TARGETS))];
    } else {
      fields = [field('Apply to', select(r, 'target', TARGETS))];
    }
    const move = (delta) => {
      const j = i + delta;
      if (j < 0 || j >= rules.length) return;
      [rules[i], rules[j]] = [rules[j], rules[i]];
      renderRules();
      schedule();
    };
    return el('section', { class: 'rule', 'aria-label': `Rule ${i + 1}: ${RULE_LABEL[r.type]}` },
      el('header', {},
        el('strong', {}, `${i + 1}. ${RULE_LABEL[r.type]}`),
        el('span', { class: 'grow' }),
        el('button', { class: 'icon-btn sm', type: 'button', title: 'Move up', 'aria-label': 'Move rule up', disabled: i === 0, onclick: () => move(-1) }, icon('asc')),
        el('button', { class: 'icon-btn sm', type: 'button', title: 'Move down', 'aria-label': 'Move rule down', disabled: i === rules.length - 1, onclick: () => move(1) }, icon('desc')),
        el('button', {
          class: 'icon-btn sm', type: 'button', title: 'Remove rule', 'aria-label': 'Remove rule',
          onclick: () => { rules.splice(i, 1); renderRules(); schedule(); },
        }, icon('close'))),
      ...fields);
  }

  function renderRules() {
    rulesEl.replaceChildren(
      ...rules.map(ruleCard),
      ...(rules.length === 0 ? [el('p', { class: 'muted' }, 'No rules yet. Add one below.')] : []),
    );
  }

  const adders = el('div', { class: 'adders' },
    el('span', { class: 'fld-l' }, 'Add a rule'),
    ...Object.keys(RULE_LABEL).map((t) => btn(RULE_LABEL[t], {
      onclick: () => { rules.push(newRule(t)); renderRules(); schedule(); },
    })));
  const presets = el('div', { class: 'adders' },
    el('span', { class: 'fld-l' }, 'Quick starts'),
    ...PRESETS.map(([label, make]) => btn(label, {
      onclick: () => { rules.push(...make()); renderRules(); schedule(); },
    })));

  const counter = el('div', { class: 'inline counter' },
    field('Counter starts at', el('input', {
      type: 'number', class: 'field num', value: opts.counter_start,
      oninput: (e) => { opts.counter_start = e.target.value; schedule(); },
    })),
    field('Counter steps by', el('input', {
      type: 'number', class: 'field num', value: opts.counter_step,
      oninput: (e) => { opts.counter_step = e.target.value; schedule(); },
    })));

  const help = el('details', { class: 'tokens' },
    el('summary', {}, 'Tokens you can use in rule text'),
    el('dl', {},
      ...[
        ['{n} or {n:3}', 'Counter, in the order shown below. {n:3} pads to 001'],
        ['{name} {stem} {ext}', 'The original name, without extension, or the extension'],
        ['{parent}', 'Name of the containing folder'],
        ['{taken:%Y%m%d_%H%M%S}', 'Date taken from the photo’s EXIF data'],
        ['{modified:%Y-%m-%d}', 'Date modified (also {created}); uses your time zone'],
        ['{camera} {make} {model}', 'Camera details from EXIF'],
        ['{width} {height}', 'Pixel size from EXIF'],
      ].flatMap(([k, v]) => [el('dt', {}, el('code', {}, k)), el('dd', {}, v)])),
    el('p', { class: 'muted' }, 'Date codes: %Y year, %m month, %d day, %H hour, %M minute, %S second.'));

  body.append(el('div', { class: 'rn-layout' },
    el('div', { class: 'rn-left' }, rulesEl, adders, presets, counter, help),
    el('div', { class: 'rn-right' }, banner, previewEl)));
  foot.append(
    summary, el('span', { class: 'grow' }),
    btn('Cancel', { onclick: () => d.close('') }),
    apply);

  apply.addEventListener('click', async () => {
    apply.disabled = true;
    try {
      const res = await call('rename_apply', payload());
      toast(`Renamed ${plural(res.renamed, 'item')}`, 'ok');
      d.close('applied');
    } catch (e) {
      banner.textContent = e.message;
      banner.hidden = false;
      apply.disabled = false;
    }
  });

  renderRules();
  preview();
  return openDialog(d, { onOpen: () => { const f = rulesEl.querySelector('input'); if (f) f.focus(); } })
    .then((v) => v === 'applied');
}

// ---------------------------------------------------------------------------
// Find

const UNITS = { KB: 1024, MB: 1024 ** 2, GB: 1024 ** 3 };

function dayStart(v) { return Math.floor(new Date(`${v}T00:00:00`).getTime() / 1000); }
function dayEnd(v) { return Math.floor(new Date(`${v}T23:59:59`).getTime() / 1000); }
function exifDay(v, end) {
  const [y, m, dd] = v.split('-').map(Number);
  return end
    ? { year: y, month: m, day: dd, hour: 23, minute: 59, second: 59 }
    : { year: y, month: m, day: dd, hour: 0, minute: 0, second: 0 };
}

export function findDialog({ root, showHidden }) {
  const { d, body, foot } = shell('Find files', 'wide find');
  const form = el('form', { class: 'find-form', id: 'find-form', novalidate: true });
  const err = el('p', { class: 'err', hidden: true, role: 'alert' });
  const inp = (props) => el('input', { class: 'field', spellcheck: 'false', autocomplete: 'off', ...props });
  const f = {
    root: inp({ type: 'text', value: root }),
    name: inp({ type: 'text', placeholder: 'part of the name, *.jpg, or a pattern' }),
    mode: el('select', { class: 'field' },
      el('option', { value: 'Substring' }, 'Contains'),
      el('option', { value: 'Glob' }, 'Wildcard (* and ?)'),
      el('option', { value: 'Regex' }, 'Regular expression')),
    icase: el('input', { type: 'checkbox', checked: true }),
    kind: el('select', { class: 'field' },
      el('option', { value: 'Any' }, 'Files and folders'),
      el('option', { value: 'File' }, 'Files only'),
      el('option', { value: 'Dir' }, 'Folders only')),
    ext: inp({ type: 'text', placeholder: 'jpg, png, heic' }),
    minV: inp({ type: 'number', min: '0', step: 'any', class: 'field num' }),
    minU: el('select', { class: 'field unit' }, Object.keys(UNITS).map((u) => el('option', { value: u, selected: u === 'MB' }, u))),
    maxV: inp({ type: 'number', min: '0', step: 'any', class: 'field num' }),
    maxU: el('select', { class: 'field unit' }, Object.keys(UNITS).map((u) => el('option', { value: u, selected: u === 'MB' }, u))),
    modA: inp({ type: 'date' }),
    modB: inp({ type: 'date' }),
    takA: inp({ type: 'date' }),
    takB: inp({ type: 'date' }),
    content: inp({ type: 'text', placeholder: 'text inside files' }),
    cre: el('input', { type: 'checkbox' }),
    sub: el('input', { type: 'checkbox', checked: true }),
    hid: el('input', { type: 'checkbox', checked: !!showHidden }),
    limit: inp({ type: 'number', min: '1', value: '5000', class: 'field num' }),
  };
  const row = (label, ...c) => el('div', { class: 'frow' }, el('span', { class: 'fld-l' }, label), el('div', { class: 'fcells' }, ...c));
  const chk = (input, label) => el('label', { class: 'chk' }, input, label);

  form.append(
    row('Look in', f.root),
    row('Name', f.name, f.mode, chk(f.icase, 'Ignore case')),
    row('Type', f.kind, f.ext),
    row('Larger than', f.minV, f.minU, el('span', { class: 'fld-l inl' }, 'Smaller than'), f.maxV, f.maxU),
    row('Modified between', f.modA, el('span', { class: 'fld-l inl' }, 'and'), f.modB),
    row('Date taken between', f.takA, el('span', { class: 'fld-l inl' }, 'and'), f.takB),
    row('Contains text', f.content, chk(f.cre, 'Regular expression')),
    row('Options', chk(f.sub, 'Search subfolders'), chk(f.hid, 'Include hidden items'),
      el('span', { class: 'fld-l inl' }, 'Stop after'), f.limit, el('span', { class: 'fld-l inl' }, 'results')),
  );
  body.append(form, err);

  let busy = false;
  const stopBtn = btn('Stop', { kind: 'danger', onclick: () => call('cancel_search').catch(() => {}) });
  stopBtn.hidden = true;
  // The footer is outside the <form>, so the button must name it, or Enter/click would do nothing.
  const go = btn('Find', { kind: 'primary', type: 'submit', form: 'find-form' });
  const spinner = el('span', { class: 'muted', hidden: true, role: 'status' }, 'Searching…');
  foot.append(spinner, el('span', { class: 'grow' }), btn('Cancel', { onclick: () => d.close('') }), stopBtn, go);

  const fail = (m) => { err.textContent = m; err.hidden = false; };
  let result = null;

  function build() {
    const q = {
      root: f.root.value.trim(),
      name: f.name.value || null,
      name_mode: f.mode.value,
      ignore_case: f.icase.checked,
      kind: f.kind.value,
      extensions: f.ext.value.split(/[\s,;]+/).filter(Boolean),
      include_hidden: f.hid.checked,
      max_results: Math.max(1, parseInt(f.limit.value, 10) || 5000),
    };
    if (!q.root) throw new Error('Choose a folder to look in.');
    const num = (v, u, label) => {
      const n = parseFloat(v);
      if (!Number.isFinite(n) || n < 0) throw new Error(`${label} must be a number.`);
      return Math.round(n * UNITS[u]);
    };
    if (f.minV.value !== '') q.min_size = num(f.minV.value, f.minU.value, 'Larger than');
    if (f.maxV.value !== '') q.max_size = num(f.maxV.value, f.maxU.value, 'Smaller than');
    if (f.modA.value) q.modified_after = dayStart(f.modA.value);
    if (f.modB.value) q.modified_before = dayEnd(f.modB.value);
    if (f.takA.value) q.taken_after = exifDay(f.takA.value, false);
    if (f.takB.value) q.taken_before = exifDay(f.takB.value, true);
    if (f.content.value) { q.content = f.content.value; q.content_regex = f.cre.checked; }
    if (!f.sub.checked) q.max_depth = 0;
    return q;
  }

  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    if (busy) return;
    err.hidden = true;
    let q;
    try { q = build(); } catch (ex) { fail(ex.message); return; }
    busy = true;
    go.hidden = true;
    stopBtn.hidden = false;
    spinner.hidden = false;
    try {
      const outcome = await call('search', { query: q });
      result = { query: q, outcome };
      if (outcome.cancelled) toast('Search stopped. Showing what was found so far.');
      d.close('done');
    } catch (ex) {
      fail(ex.message);
      busy = false;
      go.hidden = false;
      stopBtn.hidden = true;
      spinner.hidden = true;
    }
  });
  d.addEventListener('close', () => { if (busy && !result) call('cancel_search').catch(() => {}); });

  return openDialog(d, { onOpen: () => { f.name.focus(); } }).then(() => result);
}

// ---------------------------------------------------------------------------
// Compare and sync

const DIFF_LABEL = {
  LeftOnly: 'Only on left', RightOnly: 'Only on right', LeftNewer: 'Left is newer',
  RightNewer: 'Right is newer', Different: 'Different, same time', TypeMismatch: 'File vs folder',
};
const ACTION_LABEL = {
  CopyToRight: 'Copy to right', CopyToLeft: 'Copy to left',
  DeleteRight: 'Remove from right', DeleteLeft: 'Remove from left',
};
const MODES = [
  ['Update', 'Update: copy new and newer items from left to right',
    'Never removes anything and never overwrites a newer file on the right.'],
  ['Mirror', 'Mirror: make right identical to left',
    'Also removes items that exist only on the right. They go to the holding area, so Undo restores them.'],
  ['TwoWay', 'Two-way: newest wins, copy missing items both ways',
    'Cannot tell a deletion from a new file, so it never removes anything.'],
];

export function syncDialog({ left, right }) {
  const { d, body, foot } = shell('Compare and sync', 'wide sync');
  let L = left, R = right;
  let seq = 0;
  let plan = null;
  let changed = false;
  let auto = null;

  const leftEl = el('div', { class: 'pathline' });
  const rightEl = el('div', { class: 'pathline' });
  const swap = el('button', {
    class: 'icon-btn', type: 'button', title: 'Swap left and right', 'aria-label': 'Swap left and right',
    onclick: () => { [L, R] = [R, L]; paint(); compare(); },
  }, icon('swap'));
  const mode = el('select', { class: 'field', 'aria-label': 'Sync mode', onchange: () => { help(); compare(); } },
    MODES.map(([v, l]) => el('option', { value: v }, l)));
  const method = el('select', { class: 'field', 'aria-label': 'Compare by', onchange: () => compare() },
    el('option', { value: 'Quick' }, 'Compare by size and modified time (fast)'),
    el('option', { value: 'Content' }, 'Compare file contents (slower, exact)'));
  const modeHelp = el('p', { class: 'muted' });
  const help = () => { modeHelp.textContent = MODES.find((m) => m[0] === mode.value)[2]; };
  const banner = el('p', { class: 'err', hidden: true, role: 'alert' });
  const table = el('div', { class: 'sync-table', role: 'table' });
  const summary = el('span', { class: 'summary', role: 'status' }, 'Comparing…');
  const run = btn('Run sync', { kind: 'primary', disabled: true });
  const autoBtn = btn('Keep in sync automatically');
  autoBtn.title = 'Close this window and keep re-running this sync every few seconds until you stop it';

  const paint = () => { leftEl.textContent = L; rightEl.textContent = R; };

  body.append(
    el('div', { class: 'sync-top' },
      el('div', {}, el('span', { class: 'fld-l' }, 'Left'), leftEl),
      swap,
      el('div', {}, el('span', { class: 'fld-l' }, 'Right'), rightEl)),
    el('div', { class: 'sync-opts' }, mode, method),
    modeHelp, banner, table);
  foot.append(summary, el('span', { class: 'grow' }), btn('Close', { onclick: () => d.close(changed ? 'changed' : '') }), autoBtn, run);

  const side = (e) => (e ? `${e.kind === 'Dir' ? 'folder' : formatSize(e.size)}  ${formatDate(e.modified)}` : '');

  async function compare() {
    const my = ++seq;
    summary.textContent = 'Comparing…';
    run.disabled = true;
    try {
      const res = await call('compare', {
        left: L, right: R, mode: mode.value,
        options: { method: method.value, mtime_tolerance_secs: 2, include_hidden: true },
      });
      if (my !== seq) return;
      banner.hidden = true;
      plan = res.plan;
      render(res);
    } catch (e) {
      if (my !== seq) return;
      banner.textContent = e.message;
      banner.hidden = false;
      table.replaceChildren();
      summary.textContent = '';
    }
  }

  function render(res) {
    const actions = new Map(res.plan.actions.map((a) => [a.rel, a]));
    const skipped = new Map(res.plan.skipped);
    const MAX = 1000;
    const head = el('div', { class: 'srow shead', role: 'row' },
      el('span', {}, 'Item'), el('span', {}, 'Left'), el('span', {}, 'Right'), el('span', {}, 'Difference'), el('span', {}, 'What sync will do'));
    const rows = res.items.slice(0, MAX).map((it) => {
      const a = actions.get(it.rel);
      const s = skipped.get(it.rel);
      const what = a ? ACTION_LABEL[a.kind] : (s ? `Skipped: ${s}` : 'Nothing');
      const danger = a && (a.kind === 'DeleteRight' || a.kind === 'DeleteLeft');
      return el('div', { class: 'srow' + (s ? ' skip' : '') + (danger ? ' danger' : ''), role: 'row' },
        el('span', { class: 'rel', title: it.rel }, it.rel),
        el('span', { class: 'side' }, side(it.left)),
        el('span', { class: 'side' }, side(it.right)),
        el('span', {}, DIFF_LABEL[it.status] || it.status),
        el('span', { class: 'what' }, what));
    });
    table.replaceChildren(
      res.items.length ? head : el('p', { class: 'muted pad' }, `Nothing to do. ${plural(res.same_count, 'file')} are identical.`),
      ...rows,
      ...(res.items.length > MAX ? [el('p', { class: 'muted pad' }, `Showing the first ${MAX} of ${res.items.length} differences.`)] : []));
    const n = res.plan.actions.length;
    const dels = res.plan.actions.filter((a) => a.kind.startsWith('Delete')).length;
    const parts = [`${plural(n, 'action')} planned`];
    if (dels) parts.push(`${plural(dels, 'removal')}`);
    if (res.plan.skipped.length) parts.push(`${plural(res.plan.skipped.length, 'item')} skipped`);
    parts.push(`${plural(res.same_count, 'file')} identical`);
    summary.textContent = parts.join(', ');
    run.disabled = n === 0;
    run.dataset.deletes = String(dels);
  }

  autoBtn.addEventListener('click', async () => {
    const ok = await confirmDialog({
      title: 'Keep these folders in sync automatically?',
      body: el('p', {}, mode.value === 'Mirror'
        ? 'Every few seconds, Twinpane will make the right folder identical to the left, including removing items that exist only on the right. Removed items go to the holding area and each automatic sync is one undo step. Stop it any time from the toolbar.'
        : 'Every few seconds, Twinpane will run this sync if anything changed. Each automatic sync is one undo step. Stop it any time from the toolbar.'),
      confirmLabel: 'Start auto-sync', danger: mode.value === 'Mirror',
    });
    if (!ok) return;
    auto = { left: L, right: R, mode: mode.value, method: method.value };
    d.close('auto');
  });

  run.addEventListener('click', async () => {
    const dels = Number(run.dataset.deletes || 0);
    if (dels > 0) {
      const ok = await confirmDialog({
        title: `Remove ${plural(dels, 'item')} from the right side?`,
        body: el('p', {}, 'They are moved to the holding area, so Undo brings them back.'),
        confirmLabel: 'Run sync', danger: true,
      });
      if (!ok) return;
    }
    run.disabled = true;
    try {
      const rep = await call('sync_apply', { left: L, right: R, plan });
      changed = true;
      if (rep.errors.length) {
        d.close('changed');
        await reportDialog({ title: `Synced ${plural(rep.done.length, 'item')}`, skipped: rep.skipped, errors: rep.errors });
      } else {
        toast(`Synced ${plural(plan.actions.length, 'action')}. Undo reverses the whole sync.`, 'ok', 5500);
        d.close('changed');
      }
    } catch (e) {
      banner.textContent = e.message;
      banner.hidden = false;
      run.disabled = false;
    }
  });

  paint();
  help();
  compare();
  return openDialog(d, { onOpen: () => mode.focus() }).then(() => ({ changed, auto }));
}
