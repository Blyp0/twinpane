import { call, isPreview } from './api.js';
import {
  $, el, icon, hint, formatSize, formatDate, formatTaken, basename, dirname,
  plural, toast, naturalCompare,
} from './util.js';
import * as dlg from './dialogs.js';

const ROW_H = 26;
const IMAGE_EXT = new Set([
  'jpg', 'jpeg', 'jpe', 'png', 'gif', 'webp', 'heic', 'heif', 'avif', 'tif', 'tiff', 'bmp', 'svg',
  'cr2', 'cr3', 'nef', 'arw', 'dng',
]);

// Column catalogue. `sort` is what the engine understands (RowSort).
const COLS = {
  name:     { label: 'Name',       width: 'minmax(200px, 1fr)', sort: { Basic: 'Name' } },
  where:    { label: 'Folder',     width: 'minmax(140px, 0.8fr)' },
  match:    { label: 'Match',      width: 'minmax(140px, 1fr)' },
  size:     { label: 'Size',       width: '86px',  num: true, sort: { Basic: 'Size' } },
  modified: { label: 'Modified',   width: '140px', sort: { Basic: 'Modified' } },
  taken:    { label: 'Date taken', width: '140px', sort: 'DateTaken' },
  camera:   { label: 'Camera',     width: '150px', sort: 'Camera' },
  ext:      { label: 'Type',       width: '70px',  sort: { Basic: 'Extension' } },
};
const OPTIONAL_COLS = ['size', 'modified', 'taken', 'camera', 'ext'];

const PREFS_KEY = 'twinpane.prefs.v1';
const state = {
  active: 0,
  showHidden: false,
  cols: { size: true, modified: true, taken: false, camera: false, ext: false },
  home: '',
  roots: [],
  undoLast: null,
};
const panes = [];
let dragState = null;

function loadPrefs() {
  try {
    const p = JSON.parse(localStorage.getItem(PREFS_KEY) || '{}');
    if (p.cols) Object.assign(state.cols, p.cols);
    if (typeof p.showHidden === 'boolean') state.showHidden = p.showHidden;
  } catch { /* storage unavailable: use defaults */ }
}
function savePrefs() {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify({ cols: state.cols, showHidden: state.showHidden }));
  } catch { /* ignore */ }
}

const active = () => panes[state.active];
const other = () => panes[1 - state.active];

async function guarded(fn) {
  try { return await fn(); } catch (e) { toast(e.message || String(e), 'error', 7000); return undefined; }
}

function cameraLabel(meta) {
  if (!meta) return '';
  const { make, model } = meta;
  if (make && model) return model.toLowerCase().startsWith(make.toLowerCase()) ? model : `${make} ${model}`;
  return model || make || '';
}

function iconFor(entry) {
  if (entry.kind === 'Dir') return 'folder';
  if (entry.kind === 'Symlink') return 'link';
  return IMAGE_EXT.has(entry.extension || '') ? 'image' : 'file';
}

let tabSeq = 1;
function newTab(path) {
  return {
    id: tabSeq++, kind: 'dir', path, rows: [], sort: { col: 'name', dir: 'Asc' },
    cursor: 0, anchor: 0, selected: new Set(), history: [path], hi: 0,
    query: null, truncated: false, hasContent: false, stale: false, loaded: false, scrollTop: 0,
  };
}

// ---------------------------------------------------------------------------
class Pane {
  constructor(index) {
    this.index = index;
    this.tabs = [];
    this.ti = 0;

    this.root = el('section', { class: 'pane', dataset: { pane: index }, 'aria-label': `Pane ${index + 1}` });
    this.tabsEl = el('div', { class: 'tabs', role: 'tablist' });
    this.upBtn = el('button', {
      class: 'icon-btn', title: 'Parent folder (Backspace)', 'aria-label': 'Parent folder',
      onclick: () => this.up(),
    }, icon('up'));
    this.places = el('select', {
      class: 'places', 'aria-label': 'Go to place',
      onchange: () => {
        const v = this.places.value;
        this.places.value = '';
        if (v) this.go(v);
      },
    });
    this.pathInput = el('input', {
      class: 'path', type: 'text', spellcheck: 'false', autocomplete: 'off', 'aria-label': 'Folder path',
      onkeydown: (e) => {
        if (e.key === 'Enter') { this.go(this.pathInput.value.trim()); this.focusList(); }
        else if (e.key === 'Escape') { this.renderBar(); this.focusList(); }
        e.stopPropagation();
      },
    });
    this.headEl = el('div', { class: 'head', role: 'row' });
    this.listEl = el('div', {
      class: 'list', tabindex: '0', role: 'listbox',
      'aria-multiselectable': 'true', 'aria-label': `Files in pane ${index + 1}`,
    });
    this.spacer = el('div', { class: 'spacer' });
    this.rowsEl = el('div', { class: 'rows' });
    this.emptyEl = el('div', { class: 'empty', hidden: true });
    this.listEl.append(this.spacer, this.rowsEl, this.emptyEl);

    this.root.append(
      this.tabsEl,
      el('div', { class: 'bar' }, this.upBtn, this.places, this.pathInput),
      this.headEl,
      this.listEl,
    );

    this.listEl.addEventListener('scroll', () => {
      if (this.tab) this.tab.scrollTop = this.listEl.scrollTop;
      this.renderRows();
    });
    new ResizeObserver(() => this.renderRows()).observe(this.listEl);
    this.listEl.addEventListener('focus', () => { if (state.active !== index) setActive(index, false); });
    this.root.addEventListener('mousedown', (e) => {
      setActive(index, !e.target.closest('input,select,button'));
    }, true);
    this.listEl.addEventListener('mousedown', (e) => {
      if (e.button !== 0) return;
      const row = e.target.closest('.row');
      if (!row) return;
      this.pick(+row.dataset.i, { ctrl: e.ctrlKey || e.metaKey, shift: e.shiftKey });
    });
    this.listEl.addEventListener('dblclick', (e) => {
      const row = e.target.closest('.row');
      if (row) this.openRow(this.tab.rows[+row.dataset.i]);
    });
    this.initDrag();
  }

  get tab() { return this.tabs[this.ti]; }
  focusList() { this.listEl.focus({ preventScroll: true }); }

  // ---- rendering ----------------------------------------------------------
  colsFor(t) {
    if (t.kind === 'results') {
      const c = ['name', 'where'];
      if (t.hasContent) c.push('match');
      c.push('size', 'modified');
      return c;
    }
    return ['name', ...OPTIONAL_COLS.filter((k) => state.cols[k])];
  }

  renderAll() {
    this.renderTabs();
    this.renderBar();
    this.renderHead();
    this.renderRows();
    if (state.active === this.index) updateStatus();
  }

  renderTabs() {
    const items = this.tabs.map((t, i) => {
      const title = t.kind === 'results'
        ? `Find: ${t.label}`
        : (basename(t.path) || t.path);
      return el('div', {
        class: 'tab' + (i === this.ti ? ' on' : ''), role: 'tab', 'aria-selected': String(i === this.ti),
        title: t.kind === 'results' ? `Search results in ${t.query.root}` : t.path,
        onclick: () => this.switchTab(i),
      },
      t.kind === 'results' ? icon('search') : null,
      el('span', { class: 'tab-title' }, title),
      this.tabs.length > 1
        ? el('button', {
          class: 'tab-x', 'aria-label': `Close tab ${title}`, title: 'Close tab (Ctrl+W)',
          onclick: (e) => { e.stopPropagation(); this.closeTab(i); },
        }, icon('close'))
        : null);
    });
    this.tabsEl.replaceChildren(
      ...items,
      el('button', {
        class: 'tab-new', title: 'New tab (Ctrl+T)', 'aria-label': 'New tab',
        onclick: () => this.addTab(this.tab.kind === 'dir' ? this.tab.path : this.tab.query.root),
      }, icon('plus')),
    );
  }

  renderBar() {
    const t = this.tab;
    if (!t) return;
    const isDir = t.kind === 'dir';
    this.pathInput.value = isDir ? t.path : t.query.root;
    this.pathInput.readOnly = !isDir;
    this.upBtn.disabled = !isDir || dirname(t.path) === t.path;
    const opts = [el('option', { value: '' }, 'Places')];
    if (state.home) opts.push(el('option', { value: state.home }, 'Home'));
    for (const r of state.roots) if (r !== state.home) opts.push(el('option', { value: r }, r));
    this.places.replaceChildren(...opts);
  }

  renderHead() {
    const t = this.tab;
    if (!t) return;
    const cols = this.colsFor(t);
    this.root.style.setProperty('--cols', cols.map((c) => COLS[c].width).join(' '));
    this.headEl.replaceChildren(...cols.map((c) => {
      const def = COLS[c];
      const sortable = t.kind === 'results' ? ['name', 'where', 'size', 'modified'].includes(c) : !!def.sort;
      const on = t.sort.col === c;
      return el('button', {
        class: 'th' + (def.num ? ' num' : '') + (on ? ' on' : ''), disabled: !sortable,
        'aria-sort': on ? (t.sort.dir === 'Asc' ? 'ascending' : 'descending') : 'none',
        onclick: () => this.setSort(c),
      }, def.label, on ? icon(t.sort.dir === 'Asc' ? 'asc' : 'desc', 'sort') : null);
    }));
  }

  renderRows() {
    const t = this.tab;
    if (!t) return;
    const total = t.rows.length;
    this.spacer.style.height = `${total * ROW_H}px`;
    const vh = this.listEl.clientHeight || 400;
    const st = this.listEl.scrollTop;
    const start = Math.max(0, Math.floor(st / ROW_H) - 6);
    const end = Math.min(total, Math.ceil((st + vh) / ROW_H) + 6);
    this.rowsEl.style.transform = `translateY(${start * ROW_H}px)`;
    const frag = document.createDocumentFragment();
    for (let i = start; i < end; i++) frag.append(this.rowEl(t, i));
    this.rowsEl.replaceChildren(frag);

    if (total === 0) {
      this.emptyEl.hidden = false;
      this.emptyEl.textContent = t.kind === 'results'
        ? 'Nothing matched. Loosen the filters and search again.'
        : (t.loaded ? 'This folder is empty.' : 'Loading…');
    } else {
      this.emptyEl.hidden = true;
    }
  }

  rowEl(t, i) {
    const row = t.rows[i];
    const e = row.entry;
    const selected = t.selected.has(e.path);
    const cols = this.colsFor(t);
    return el('div', {
      class: 'row' + (selected ? ' sel' : '') + (i === t.cursor ? ' cursor' : '') + (e.hidden ? ' hid' : ''),
      role: 'option', 'aria-selected': String(selected), draggable: 'true',
      dataset: { i, dir: e.kind === 'Dir' ? '1' : '' },
      title: e.path,
    }, cols.map((c) => this.cell(c, row, t)));
  }

  cell(col, row, t) {
    const e = row.entry;
    switch (col) {
      case 'name':
        return el('div', { class: 'cell name' },
          icon(iconFor(e), 'k-' + e.kind.toLowerCase()), el('span', { class: 'nm' }, e.name));
      case 'where': {
        const root = t.query ? t.query.root : '';
        let d = dirname(e.path);
        if (root && d.startsWith(root)) d = d.slice(root.length).replace(/^[\\/]+/, '') || '.';
        return el('div', { class: 'cell where' }, d);
      }
      case 'match':
        return el('div', { class: 'cell match' }, row.line ? `${row.line}: ${row.snippet || ''}` : '');
      case 'size':
        return el('div', { class: 'cell num' }, e.kind === 'Dir' ? '' : formatSize(e.size));
      case 'modified':
        return el('div', { class: 'cell' }, formatDate(e.modified));
      case 'taken':
        return el('div', { class: 'cell' }, formatTaken(row.meta && row.meta.date_taken));
      case 'camera':
        return el('div', { class: 'cell' }, cameraLabel(row.meta));
      case 'ext':
        return el('div', { class: 'cell' }, e.extension || '');
      default:
        return el('div', { class: 'cell' });
    }
  }

  /** Update highlight classes on the rows already on screen, without
   *  recreating them (recreating a row mid-click breaks double-click and drag). */
  paintSelection() {
    const t = this.tab;
    for (const r of this.rowsEl.children) {
      const i = +r.dataset.i;
      const row = t.rows[i];
      if (!row) continue;
      const sel = t.selected.has(row.entry.path);
      r.classList.toggle('sel', sel);
      r.classList.toggle('cursor', i === t.cursor);
      r.setAttribute('aria-selected', String(sel));
    }
  }

  // ---- selection & cursor -------------------------------------------------
  pick(i, { ctrl = false, shift = false } = {}) {
    const t = this.tab;
    if (i < 0 || i >= t.rows.length) return;
    const path = t.rows[i].entry.path;
    if (shift) {
      const [a, b] = [Math.min(t.anchor, i), Math.max(t.anchor, i)];
      t.selected = new Set(t.rows.slice(a, b + 1).map((r) => r.entry.path));
    } else if (ctrl) {
      if (t.selected.has(path)) t.selected.delete(path); else t.selected.add(path);
      t.anchor = i;
    } else {
      t.selected = new Set([path]);
      t.anchor = i;
    }
    t.cursor = i;
    this.paintSelection();
    updateStatus();
  }

  /** mode: 'select' (single selection follows), 'extend' (range), 'cursor' (move only) */
  moveCursor(delta, { abs = null, mode = 'select' } = {}) {
    const t = this.tab;
    const n = t.rows.length;
    if (!n) return;
    const i = Math.max(0, Math.min(n - 1, abs != null ? abs : t.cursor + delta));
    if (mode === 'extend') {
      const [a, b] = [Math.min(t.anchor, i), Math.max(t.anchor, i)];
      t.selected = new Set(t.rows.slice(a, b + 1).map((r) => r.entry.path));
    } else if (mode === 'select') {
      t.selected = new Set([t.rows[i].entry.path]);
      t.anchor = i;
    }
    t.cursor = i;
    this.scrollTo(i);
    this.paintSelection();
    updateStatus();
  }

  toggleCursor() {
    const t = this.tab;
    const r = t.rows[t.cursor];
    if (!r) return;
    if (t.selected.has(r.entry.path)) t.selected.delete(r.entry.path); else t.selected.add(r.entry.path);
    t.anchor = t.cursor;
    this.paintSelection();
    updateStatus();
  }

  selectAll() {
    const t = this.tab;
    t.selected = new Set(t.rows.map((r) => r.entry.path));
    this.paintSelection();
    updateStatus();
  }

  clearSelection() {
    this.tab.selected = new Set();
    this.paintSelection();
    updateStatus();
  }

  scrollTo(i) {
    const top = i * ROW_H;
    const bottom = top + ROW_H;
    const st = this.listEl.scrollTop;
    const vh = this.listEl.clientHeight;
    if (top < st) this.listEl.scrollTop = top;
    else if (bottom > st + vh) this.listEl.scrollTop = bottom - vh;
  }

  /** The rows an action applies to: the selection, or else the cursor row. */
  targetRows() {
    const t = this.tab;
    if (t.selected.size) return t.rows.filter((r) => t.selected.has(r.entry.path));
    const r = t.rows[t.cursor];
    return r ? [r] : [];
  }

  // ---- loading & navigation -------------------------------------------------
  list(path, tab) {
    const needMeta = state.cols.taken || state.cols.camera || ['taken', 'camera'].includes(tab.sort.col);
    return call('list_rows', {
      path, show_hidden: state.showHidden, sort: COLS[tab.sort.col].sort,
      order: tab.sort.dir, with_meta: needMeta,
    });
  }

  async load(tab = this.tab, { select = null } = {}) {
    if (tab.kind === 'results') { await this.rerun(tab); return; }
    const keep = tab.rows[tab.cursor] && tab.rows[tab.cursor].entry.path;
    try {
      const res = await this.list(tab.path, tab);
      tab.path = res.path;
      tab.rows = res.rows;
      tab.stale = false;
      tab.loaded = true;
      const have = new Set(tab.rows.map((r) => r.entry.path));
      tab.selected = new Set([...tab.selected].filter((p) => have.has(p)));
      const want = select || keep;
      let idx = want ? tab.rows.findIndex((r) => r.entry.path === want) : -1;
      if (idx < 0) idx = Math.min(tab.cursor, Math.max(0, tab.rows.length - 1));
      tab.cursor = idx;
      tab.anchor = idx;
      if (select && have.has(select)) tab.selected = new Set([select]);
    } catch (e) {
      toast(`Couldn't read ${tab.path}: ${e.message}`, 'error', 6000);
      tab.loaded = true;
    }
    if (tab === this.tab) {
      this.renderAll();
      if (select) this.scrollTo(tab.cursor);
    }
  }

  async go(path, { select = null, push = true } = {}) {
    if (!path) return false;
    if (this.tab.kind !== 'dir') { await this.addTab(path, { select }); return true; }
    const t = this.tab;
    try {
      const res = await this.list(path, t);
      if (push) {
        t.history = t.history.slice(0, t.hi + 1);
        t.history.push(res.path);
        t.hi = t.history.length - 1;
      }
      t.path = res.path;
      t.rows = res.rows;
      t.stale = false;
      t.loaded = true;
      t.selected = new Set();
      t.cursor = 0;
      t.anchor = 0;
      this.listEl.scrollTop = 0;
      if (select) {
        const i = t.rows.findIndex((r) => r.entry.path === select);
        if (i >= 0) { t.cursor = i; t.anchor = i; t.selected = new Set([select]); }
      }
      this.renderAll();
      if (select) this.scrollTo(t.cursor);
      return true;
    } catch (e) {
      toast(`Couldn't open ${path}: ${e.message}`, 'error', 6000);
      this.renderBar();
      return false;
    }
  }

  up() {
    const t = this.tab;
    if (t.kind !== 'dir') return;
    const p = dirname(t.path);
    if (p !== t.path) this.go(p, { select: t.path });
  }

  back() {
    const t = this.tab;
    if (t.kind === 'dir' && t.hi > 0) { t.hi--; this.go(t.history[t.hi], { push: false }); }
  }

  forward() {
    const t = this.tab;
    if (t.kind === 'dir' && t.hi < t.history.length - 1) { t.hi++; this.go(t.history[t.hi], { push: false }); }
  }

  async openRow(row) {
    if (!row) return;
    const e = row.entry;
    if (this.tab.kind === 'results') {
      await this.addTab(e.kind === 'Dir' ? e.path : dirname(e.path), { select: e.kind === 'Dir' ? null : e.path });
      return;
    }
    if (e.kind === 'Dir') { await this.go(e.path); return; }
    if (e.kind === 'Symlink' && await this.go(e.path)) return;
    await guarded(() => call('open', { path: e.path }));
  }

  // ---- tabs -----------------------------------------------------------------
  async addTab(path, { select = null } = {}) {
    const t = newTab(path);
    this.tabs.splice(this.ti + 1, 0, t);
    this.ti += 1;
    this.listEl.scrollTop = 0;
    this.renderAll();
    await this.load(t, { select });
    this.renderAll();
  }

  closeTab(i = this.ti) {
    if (this.tabs.length === 1) return;
    this.tabs.splice(i, 1);
    if (i < this.ti) this.ti -= 1;
    else if (i === this.ti) this.ti = Math.min(i, this.tabs.length - 1);
    this.activateCurrent();
  }

  switchTab(i) {
    if (i < 0 || i >= this.tabs.length) return;
    this.ti = i;
    this.activateCurrent();
  }

  async activateCurrent() {
    const t = this.tab;
    this.renderAll();
    this.listEl.scrollTop = t.scrollTop;
    if (t.stale || !t.loaded) await this.load(t);
  }

  cycleTab(dir) {
    this.switchTab((this.ti + dir + this.tabs.length) % this.tabs.length);
  }

  // ---- sorting ----------------------------------------------------------------
  setSort(col) {
    const t = this.tab;
    if (t.sort.col === col) t.sort.dir = t.sort.dir === 'Asc' ? 'Desc' : 'Asc';
    else t.sort = { col, dir: 'Asc' };
    if (t.kind === 'results') { this.sortLocal(t); this.renderAll(); } else this.load(t);
  }

  sortLocal(t) {
    const m = t.sort.dir === 'Asc' ? 1 : -1;
    const key = {
      name: (r) => r.entry.name,
      where: (r) => dirname(r.entry.path),
      size: (r) => r.entry.size,
      modified: (r) => r.entry.modified || 0,
    }[t.sort.col];
    if (!key) return;
    t.rows.sort((a, b) => {
      const ka = key(a), kb = key(b);
      const c = typeof ka === 'string' ? naturalCompare(ka, kb) : ka - kb;
      return c * m || naturalCompare(a.entry.name, b.entry.name);
    });
  }

  // ---- search results ---------------------------------------------------------
  openResults(query, outcome) {
    const t = newTab(query.root);
    t.kind = 'results';
    t.query = query;
    t.label = query.name || query.content || 'all files';
    t.sort = { col: null, dir: 'Asc' };
    this.applyOutcome(t, outcome);
    this.tabs.splice(this.ti + 1, 0, t);
    this.ti += 1;
    this.listEl.scrollTop = 0;
    this.renderAll();
    this.focusList();
  }

  applyOutcome(t, outcome) {
    t.rows = outcome.hits.map((h) => ({ entry: h.entry, meta: null, line: h.line, snippet: h.snippet }));
    t.truncated = outcome.truncated;
    t.hasContent = !!(t.query.content);
    t.loaded = true;
    t.stale = false;
    t.selected = new Set();
    t.cursor = 0;
    t.anchor = 0;
    if (t.sort.col) this.sortLocal(t);
  }

  async rerun(t) {
    const out = await guarded(() => call('search', { query: t.query }));
    if (out) this.applyOutcome(t, out);
    if (t === this.tab) this.renderAll();
  }

  // ---- drag and drop between panes ------------------------------------------------
  initDrag() {
    const l = this.listEl;
    const clearDrop = () => l.querySelectorAll('.row.drop').forEach((r) => r.classList.remove('drop'));
    l.addEventListener('dragstart', (e) => {
      const row = e.target.closest('.row');
      if (!row) return;
      const t = this.tab;
      const i = +row.dataset.i;
      if (!t.selected.has(t.rows[i].entry.path)) this.pick(i, {});
      const rows = this.targetRows();
      dragState = { pane: this.index, rows };
      e.dataTransfer.effectAllowed = 'copyMove';
      e.dataTransfer.setData('text/plain', rows.map((r) => r.entry.path).join('\n'));
    });
    l.addEventListener('dragover', (e) => {
      if (!dragState) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = (e.ctrlKey || e.metaKey) ? 'copy' : 'move';
      clearDrop();
      const row = e.target.closest('.row');
      if (row && row.dataset.dir && this.tab.kind === 'dir') row.classList.add('drop');
    });
    l.addEventListener('dragleave', (e) => { if (e.target === l) clearDrop(); });
    l.addEventListener('dragend', () => { dragState = null; clearDrop(); });
    l.addEventListener('drop', (e) => {
      if (!dragState) return;
      e.preventDefault();
      const d = dragState;
      dragState = null;
      clearDrop();
      const row = e.target.closest('.row');
      const t = this.tab;
      let dest = null;
      if (row && row.dataset.dir && t.kind === 'dir') dest = t.rows[+row.dataset.i].entry.path;
      else if (t.kind === 'dir' && d.pane !== this.index) dest = t.path;
      if (!dest) return;
      transfer((e.ctrlKey || e.metaKey) ? 'copy' : 'move', d.rows, dest);
    });
  }
}

// ---------------------------------------------------------------------------
function setActive(i, focus = true) {
  state.active = i;
  panes.forEach((p, k) => p.root.classList.toggle('active', k === i));
  updateStatus();
  if (focus) panes[i].focusList();
}

function updateStatus() {
  const p = active();
  if (!p || !p.tab) return;
  const t = p.tab;
  const n = t.rows.length;
  const sel = t.rows.filter((r) => t.selected.has(r.entry.path));
  let text;
  if (sel.length) {
    const bytes = sel.reduce((s, r) => s + (r.entry.kind === 'Dir' ? 0 : r.entry.size), 0);
    text = `${sel.length} of ${plural(n, 'item')} selected, ${formatSize(bytes)}`;
  } else if (t.kind === 'results') {
    text = plural(n, 'result') + (t.truncated ? ' (stopped at the result limit)' : '');
  } else {
    text = plural(n, 'item');
  }
  $('#st-sel').textContent = text;
}

function updateUndoUI() {
  $('#st-undo').textContent = state.undoLast ? `Undo: ${state.undoLast}` : 'Nothing to undo';
  const b = document.getElementById('act-undo');
  if (b) b.disabled = !state.undoLast;
}

async function refreshUndo() {
  try {
    const s = await call('undo_state');
    state.undoLast = s.descriptions.length ? s.descriptions[s.descriptions.length - 1] : null;
  } catch { /* leave as is */ }
  updateUndoUI();
}

/** Reload what is on screen; other tabs reload when next shown. */
async function refreshAll({ select = null } = {}) {
  await Promise.all(panes.map((p, i) => {
    p.tabs.forEach((t) => { if (t !== p.tab) t.stale = true; });
    return p.load(p.tab, { select: i === state.active ? select : null });
  }));
  await refreshUndo();
}

// ---- operations -------------------------------------------------------------
async function transfer(kind, rows, dest) {
  if (!rows.length) return;
  const choice = await dlg.transferDialog({ kind, rows, dest });
  active().focusList();
  if (!choice) return;
  const rep = await guarded(() => call(kind, {
    sources: rows.map((r) => r.entry.path), dest, policy: choice.policy,
  }));
  if (!rep) return;
  await afterOp(rep, kind === 'copy' ? 'Copied' : 'Moved');
}

async function afterOp(rep, verb) {
  const done = rep.done.length;
  if (rep.errors.length || rep.skipped.length) {
    await dlg.reportDialog({ title: `${verb} ${plural(done, 'item')}`, skipped: rep.skipped, errors: rep.errors });
  } else {
    toast(`${verb} ${plural(done, 'item')}`, 'ok');
  }
  await refreshAll();
}

function destPane() {
  const o = other();
  if (o.tab.kind !== 'dir') { toast('Open a folder in the other pane first.'); return null; }
  return o.tab.path;
}

async function doTransfer(kind) {
  const rows = active().targetRows();
  if (!rows.length) return;
  const dest = destPane();
  if (dest) await transfer(kind, rows, dest);
}

async function doDelete() {
  const p = active();
  const rows = p.targetRows();
  if (!rows.length) return;
  const names = rows.slice(0, 5).map((r) => r.entry.name);
  const more = rows.length > 5 ? `and ${rows.length - 5} more` : '';
  const ok = await dlg.confirmDialog({
    title: `Delete ${plural(rows.length, 'item')}?`,
    body: [
      el('p', {}, 'Deleted items are kept in a holding area, so Undo can bring them back.'),
      el('ul', { class: 'plain' }, names.map((n) => el('li', {}, n)), more ? el('li', { class: 'muted' }, more) : null),
    ],
    confirmLabel: 'Delete', danger: true,
  });
  p.focusList();
  if (!ok) return;
  const rep = await guarded(() => call('delete', { paths: rows.map((r) => r.entry.path) }));
  if (rep) await afterOp(rep, 'Deleted');
}

async function doRename() {
  const p = active();
  const rows = p.targetRows();
  if (!rows.length) return;
  if (rows.length > 1) { await doBatchRename(); return; }
  const row = rows[0];
  const name = await dlg.promptDialog({
    title: 'Rename', label: 'New name', value: row.entry.name,
    confirmLabel: 'Rename', stem: row.entry.kind !== 'Dir',
  });
  p.focusList();
  if (name == null || name === row.entry.name) return;
  const np = await guarded(() => call('rename', { path: row.entry.path, new_name: name }));
  if (np) await refreshAll({ select: np });
}

async function doMkdir() {
  const p = active();
  const t = p.tab;
  if (t.kind !== 'dir') { toast('Open a folder first.'); return; }
  const name = await dlg.promptDialog({ title: 'New folder', label: 'Folder name', value: 'New folder', confirmLabel: 'Create' });
  p.focusList();
  if (!name) return;
  const np = await guarded(() => call('mkdir', { parent: t.path, name }));
  if (np) await refreshAll({ select: np });
}

async function doUndo() {
  const r = await guarded(() => call('undo'));
  if (r) { toast(r.message, 'ok'); }
  await refreshAll();
}

async function doBatchRename() {
  const p = active();
  const rows = p.targetRows();
  if (!rows.length) return;
  const applied = await dlg.renameDialog({ paths: rows.map((r) => r.entry.path) });
  p.focusList();
  if (applied) await refreshAll();
}

async function doFind() {
  const p = active();
  const t = p.tab;
  const root = t.kind === 'dir' ? t.path : t.query.root;
  const res = await dlg.findDialog({ root, showHidden: state.showHidden });
  if (!res) { p.focusList(); return; }
  p.openResults(res.query, res.outcome);
}

async function doSync() {
  const a = active().tab, b = other().tab;
  if (a.kind !== 'dir' || b.kind !== 'dir') { toast('Open a folder in each pane to compare them.'); return; }
  const { changed, auto } = await dlg.syncDialog({ left: a.path, right: b.path });
  active().focusList();
  if (auto) startAutoSync(auto);
  if (changed) await refreshAll();
}

// ---- auto sync ----------------------------------------------------------------
const AUTO_SYNC_MS = 3000;
let autoSync = null;

function updateAutoSyncUI() {
  const b = document.getElementById('autosync-chip');
  if (!b) return;
  b.hidden = !autoSync;
  if (autoSync) b.title = `Auto-sync (${autoSync.mode}): ${autoSync.left} \u2192 ${autoSync.right}. Click to stop.`;
}

function startAutoSync(cfg) {
  stopAutoSync(false);
  autoSync = { ...cfg, timer: 0 };
  updateAutoSyncUI();
  toast('Auto-sync is on. Stop it from the toolbar.', 'ok');
  scheduleAutoSync(0);
}

function stopAutoSync(announce = true) {
  if (!autoSync) return;
  clearTimeout(autoSync.timer);
  autoSync = null;
  updateAutoSyncUI();
  if (announce) toast('Auto-sync stopped.');
}

function scheduleAutoSync(ms = AUTO_SYNC_MS) {
  if (!autoSync) return;
  const cfg = autoSync;
  cfg.timer = setTimeout(() => autoSyncTick(cfg), ms);
}

async function autoSyncTick(cfg) {
  if (autoSync !== cfg) return;
  // Don't change files under the user while a dialog is open.
  if (document.querySelector('dialog[open]')) { scheduleAutoSync(); return; }
  try {
    const res = await call('compare', {
      left: cfg.left, right: cfg.right, mode: cfg.mode,
      options: { method: cfg.method, mtime_tolerance_secs: 2, include_hidden: true },
    });
    if (autoSync !== cfg) return;
    if (res.plan.actions.length) {
      await call('sync_apply', { left: cfg.left, right: cfg.right, plan: res.plan });
      toast(`Auto-synced ${plural(res.plan.actions.length, 'action')}.`, 'ok', 2500);
      await refreshAll();
    }
  } catch (e) {
    stopAutoSync(false);
    toast(`Auto-sync stopped: ${e.message || e}`, 'error', 8000);
    return;
  }
  scheduleAutoSync();
}

function toggleHidden() {
  state.showHidden = !state.showHidden;
  const box = document.getElementById('opt-hidden');
  if (box) box.checked = state.showHidden;
  savePrefs();
  panes.forEach((p) => p.tabs.forEach((t) => { t.stale = true; }));
  active().load();
  other().load();
}

// ---- toolbar ----------------------------------------------------------------
function buildToolbar() {
  const tb = $('#toolbar');
  const btn = (id, label, key, ic, run) => el('button', {
    class: 'tb', id: `act-${id}`, title: `${label} (${hint(key)})`, onclick: run,
  }, icon(ic), el('span', { class: 'lbl' }, label), el('kbd', {}, hint(key)));
  const sep = () => el('span', { class: 'sep', role: 'separator' });

  const colsMenu = el('div', { class: 'menu', hidden: true, role: 'menu' });
  const colsBtn = el('button', {
    class: 'tb', title: 'Choose columns', 'aria-haspopup': 'true',
    onclick: (e) => { e.stopPropagation(); colsMenu.hidden = !colsMenu.hidden; },
  }, icon('columns'), el('span', { class: 'lbl' }, 'Columns'));
  for (const k of OPTIONAL_COLS) {
    colsMenu.append(el('label', { class: 'chk' },
      el('input', {
        type: 'checkbox', checked: state.cols[k],
        onchange: (e) => {
          state.cols[k] = e.target.checked;
          savePrefs();
          const needMeta = (k === 'taken' || k === 'camera') && e.target.checked;
          panes.forEach((p) => {
            p.tabs.forEach((t) => { if (needMeta) t.stale = true; });
            if (needMeta) p.load(); else p.renderAll();
          });
        },
      }),
      COLS[k].label));
  }
  document.addEventListener('click', () => { colsMenu.hidden = true; });

  tb.append(
    btn('copy', 'Copy', 'F5', 'copy', () => doTransfer('copy')),
    btn('move', 'Move', 'F6', 'move', () => doTransfer('move')),
    btn('mkdir', 'New folder', 'F7', 'newfolder', doMkdir),
    btn('delete', 'Delete', 'F8', 'trash', doDelete),
    sep(),
    btn('rename', 'Rename', 'F2', 'rename', doRename),
    btn('batch', 'Batch rename', 'Ctrl+R', 'batch', doBatchRename),
    sep(),
    btn('find', 'Find', 'Ctrl+F', 'search', doFind),
    btn('sync', 'Compare and sync', 'Ctrl+Shift+S', 'sync', doSync),
    sep(),
    btn('undo', 'Undo', 'Ctrl+Z', 'undo', doUndo),
    el('span', { class: 'grow' }),
    el('button', {
      class: 'tb', id: 'autosync-chip', hidden: true, onclick: () => stopAutoSync(),
    }, icon('sync'), el('span', {}, 'Auto-sync on \u00b7 Stop')),
    el('label', { class: 'chk' },
      el('input', { type: 'checkbox', id: 'opt-hidden', checked: state.showHidden, onchange: toggleHidden }),
      'Hidden files'),
    el('div', { class: 'menu-wrap' }, colsBtn, colsMenu),
  );
}

// ---- keyboard -----------------------------------------------------------------
const ta = { buf: '', t: 0 };
function typeahead(ch) {
  const now = Date.now();
  if (now - ta.t > 800) ta.buf = '';
  ta.t = now;
  ta.buf += ch.toLowerCase();
  const p = active();
  const t = p.tab;
  const n = t.rows.length;
  const start = ta.buf.length === 1 ? t.cursor + 1 : t.cursor;
  for (let k = 0; k < n; k++) {
    const i = (start + k) % n;
    if (t.rows[i].entry.name.toLowerCase().startsWith(ta.buf)) { p.moveCursor(0, { abs: i }); return; }
  }
}

function onKey(e) {
  if (document.querySelector('dialog[open]')) return;
  const tag = e.target && e.target.tagName;
  if (tag === 'INPUT' || tag === 'SELECT' || tag === 'TEXTAREA') return;
  const p = active();
  if (!p || !p.tab) return;
  const mod = e.ctrlKey || e.metaKey;
  const key = e.key;
  const pageRows = Math.max(1, Math.floor(p.listEl.clientHeight / ROW_H) - 1);
  const handled = () => { e.preventDefault(); e.stopPropagation(); };
  const mode = e.shiftKey ? 'extend' : (mod ? 'cursor' : 'select');

  if (key === 'Tab') { handled(); setActive(1 - state.active); return; }
  if (key === 'ArrowDown') { handled(); p.moveCursor(1, { mode }); return; }
  if (key === 'ArrowUp') { handled(); p.moveCursor(-1, { mode }); return; }
  if (key === 'PageDown' && !mod) { handled(); p.moveCursor(pageRows, { mode }); return; }
  if (key === 'PageUp' && !mod) { handled(); p.moveCursor(-pageRows, { mode }); return; }
  if (key === 'Home') { handled(); p.moveCursor(0, { abs: 0, mode }); return; }
  if (key === 'End') { handled(); p.moveCursor(0, { abs: p.tab.rows.length - 1, mode }); return; }
  if (key === ' ') { handled(); p.toggleCursor(); return; }
  if (key === 'Enter') { handled(); p.openRow(p.tab.rows[p.tab.cursor]); return; }
  if (key === 'Backspace') { handled(); p.up(); return; }
  if (e.altKey && key === 'ArrowLeft') { handled(); p.back(); return; }
  if (e.altKey && key === 'ArrowRight') { handled(); p.forward(); return; }
  if (key === 'Escape') { if (p.tab.selected.size) { handled(); p.clearSelection(); } return; }

  if (key === 'F2') { handled(); doRename(); return; }
  if (key === 'F5') { handled(); doTransfer('copy'); return; }
  if (key === 'F6') { handled(); doTransfer('move'); return; }
  if (key === 'F7') { handled(); doMkdir(); return; }
  if (key === 'F8' || key === 'Delete') { handled(); doDelete(); return; }

  if (mod) {
    const k = key.toLowerCase();
    if (k === 'a') { handled(); p.selectAll(); return; }
    if (k === 'z') { handled(); doUndo(); return; }
    if (k === 'r') { handled(); doBatchRename(); return; }
    if (k === 'f') { handled(); doFind(); return; }
    if (k === 's' && e.shiftKey) { handled(); doSync(); return; }
    if (k === 'h') { handled(); toggleHidden(); return; }
    if (k === 'l') { handled(); p.pathInput.focus(); p.pathInput.select(); return; }
    if (k === 't') { handled(); p.addTab(p.tab.kind === 'dir' ? p.tab.path : p.tab.query.root); return; }
    if (k === 'w') { handled(); p.closeTab(); return; }
    if (key === 'Tab' || key === 'PageDown') { handled(); p.cycleTab(e.shiftKey ? -1 : 1); return; }
    if (key === 'PageUp') { handled(); p.cycleTab(-1); return; }
    return;
  }
  if (key.length === 1 && !e.altKey) typeahead(key);
}

function showNotice(text) {
  const n = $('#notice');
  n.textContent = text;
  n.hidden = false;
}

// ---- start --------------------------------------------------------------------
async function init() {
  loadPrefs();
  buildToolbar();
  document.addEventListener('keydown', onKey);
  try {
    state.home = await call('home_dir');
    state.roots = await call('roots');
  } catch (e) {
    showNotice(`Twinpane could not start: ${e.message}`);
    return;
  }
  const host = $('#panes');
  for (const i of [0, 1]) {
    const p = new Pane(i);
    panes.push(p);
    host.append(p.root);
    p.tabs = [newTab(state.home)];
    p.ti = 0;
    p.renderAll();
  }
  await Promise.all(panes.map((p) => p.load()));
  setActive(0);
  refreshUndo();
  if (isPreview) {
    showNotice('Browser preview: you are working inside a sandbox folder, and opening files in other apps is turned off.');
  }
  // Test hook (also handy for debugging in the console).
  window.__twinpane = { panes, state, refreshAll };
}

init();
