// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

// EON Browser's window: sidebar, bar, panels. Everything it does goes through `cmd`, a
// request to a host that never resolves; the browser answers it. The browser pushes state
// with `eonState`. Where the page goes is reported with `layout`; suggestions and the
// command bar are handed to the overlay view with `overlay`.
'use strict';

const $ = (id) => document.getElementById(id);

async function cmd(op, args = {}) {
  try {
    const r = await fetch('https://ui.eon.invalid/cmd?c=' +
      encodeURIComponent(JSON.stringify(Object.assign({ op }, args))));
    return await r.json();
  } catch (_) {
    return null;
  }
}

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function icon(name, cls) {
  const img = document.createElement('img');
  img.src = 'icons/' + name + '.svg';
  img.alt = '';
  if (cls) img.className = cls;
  return img;
}

// A per-window convenience only; the browser keeps everything that matters.
function remember(key, value) { try { localStorage.setItem(key, value); } catch (_) {} }
function recall(key) { try { return localStorage.getItem(key); } catch (_) { return null; } }

let state = null;
let editing = false;
let suggestions = [];
let selected = -1;
let panel = null;
let overlayMode = 'none';

// ---- layout ---------------------------------------------------------------------------

function rect(e) {
  const r = e.getBoundingClientRect();
  return { x: r.left, y: r.top, w: r.width, h: r.height };
}

let lastLayout = '';
function reportLayout() {
  const page = rect($('page'));
  const key = JSON.stringify(page);
  if (key === lastLayout) return;
  lastLayout = key;
  cmd('layout', { page });
}

// Tabs across the top (as in every major browser) unless the person chose vertical tabs.
function setLayout(kind) {
  $('app').classList.toggle('vertical', kind === 'vertical');
  remember('eon.tabs', kind);
  reportLayout();
}

function layoutKind() {
  return $('app').classList.contains('vertical') ? 'vertical' : 'top';
}

window.addEventListener('resize', () => { reportLayout(); if (overlayMode === 'suggest') sendSuggestions(); });

// ---- panels ---------------------------------------------------------------------------

const TITLES = { shield: 'Shield', route: 'Connection', limits: 'Limiters', monitor: 'Monitor' };

function openPanel(name) {
  if (name === 'vertical' || name === 'top') { setLayout(name); return; }
  if (!TITLES[name]) return;
  if (panel === name) { closePanel(); return; }
  panel = name;
  $('panel').hidden = false;
  $('app').classList.add('with-panel');
  $('panel-title').textContent = TITLES[name];
  for (const b of document.querySelectorAll('.panel-body')) b.classList.toggle('on', b.dataset.for === name);
  if (state) renderPanel();
  reportLayout();
}

function closePanel() {
  panel = null;
  $('panel').hidden = true;
  $('app').classList.remove('with-panel');
  reportLayout();
}

window.eonOpenPanel = openPanel;

// ---- tabs -----------------------------------------------------------------------------

function renderTabs() {
  const box = $('tabs');
  box.textContent = '';
  state.tabs.forEach((t, i) => {
    const cls = 'tab' + (i === state.active ? ' active' : '') + (t.loading ? ' loading' : '') + (t.asleep ? ' asleep' : '');
    const tab = el('div', cls);
    tab.setAttribute('role', 'tab');
    tab.setAttribute('aria-selected', String(i === state.active));
    const name = t.title || t.url || 'New tab';
    tab.title = t.asleep ? name + ' (asleep: click to wake it)' : name;
    if (t.asleep && !t.icon) tab.appendChild(icon('moon', 'moon'));
    else if (t.icon && !t.loading) {
      const fav = document.createElement('img');
      fav.className = 'fav';
      fav.src = t.icon;
      fav.alt = '';
      tab.appendChild(fav);
    } else tab.appendChild(el('span', 'dot'));
    tab.appendChild(el('span', 'title', name));
    const x = el('button', 'x');
    x.title = 'Close tab (Ctrl+W)';
    x.setAttribute('aria-label', 'Close ' + name);
    x.appendChild(icon('close'));
    x.addEventListener('mousedown', (e) => { e.stopPropagation(); cmd('close', { tab: i }); });
    tab.appendChild(x);
    tab.addEventListener('mousedown', (e) => {
      if (e.button === 1) cmd('close', { tab: i });
      else cmd('select', { tab: i });
    });
    box.appendChild(tab);
  });
}

// ---- bar ------------------------------------------------------------------------------

const ROUTE_NAMES = { direct: 'Direct', tor: 'Tor', proxy: 'Proxy', socks: 'Tor proxy', none: 'Direct' };

function routeName() {
  if (state.route === 'socks' && !/127\.0\.0\.1:9[01]50$/.test(state.routeAddress || '')) return 'Proxy';
  return ROUTE_NAMES[state.route] || 'Direct';
}

function renderBar() {
  $('back').disabled = !state.canBack;
  $('forward').disabled = !state.canForward;
  if (!editing) $('address').value = state.display;

  const lock = $('lock');
  lock.className = 'lock ' + state.security;
  lock.title = {
    secure: 'The connection to this site is encrypted.',
    open: 'This site is on plain HTTP, by your choice: what you send can be read on the way.',
    page: 'A page of EON Browser.',
  }[state.security] || '';
  lock.firstElementChild.src = 'icons/' +
    (state.security === 'open' ? 'open' : state.security === 'page' ? 'page' : 'lock') + '.svg';

  const note = $('note');
  note.hidden = !state.note || editing;
  note.textContent = state.note || '';

  const shield = $('shield');
  shield.classList.toggle('off', !state.shields);
  shield.firstElementChild.src = state.shields ? 'icons/shield.svg' : 'icons/shield-off.svg';
  $('blocked').textContent = state.shields ? String(state.blocked) : 'off';
  shield.title = state.shields
    ? state.blocked + ' ads and trackers blocked on this page'
    : 'Blocking is off for this site';
  $('private').hidden = !state.private;
  const star = $('star');
  star.hidden = state.security === 'page';
  star.firstElementChild.src = state.bookmarked ? 'icons/star-on.svg' : 'icons/star.svg';
  star.title = state.bookmarked ? 'Remove the bookmark (Ctrl+D)' : 'Bookmark this page (Ctrl+D)';

  const route = $('route');
  $('route-name').textContent = routeName();
  route.className = 'route ' + (state.route === 'tor' ? 'tor ' + state.tor : state.route);
  route.title = state.route === 'tor'
    ? { connecting: 'Connecting to the Tor network…', ready: 'Through Tor: a circuit for each site.',
        failed: 'Tor could not connect.' }[state.tor] || 'Tor'
    : state.route === 'direct' || state.route === 'none' ? 'Straight to each site.' : 'Through ' + (state.routeAddress || 'a proxy');
}

// ---- panel contents -------------------------------------------------------------------

function speed(kbps) {
  if (kbps >= 1024) return (kbps / 1024).toFixed(1) + ' MB/s';
  return kbps + ' KB/s';
}

function memory(mb) {
  return mb >= 1024 ? (mb / 1024).toFixed(2) + ' GB' : mb + ' MB';
}

// Mark the button for `value` in a segmented choice; a value set elsewhere gets its own.
function choose(key, value) {
  const group = document.querySelector('.seg[data-key="' + key + '"]');
  if (!group) return;
  const v = String(value);
  if (![...group.children].some((b) => b.dataset.v === v)) {
    const b = el('button', '', key === 'sleepAfterMinutes' ? v + ' min'
      : key === 'ramLimitMb' ? memory(value) : speed(value));
    b.dataset.v = v;
    b.setAttribute('role', 'radio');
    b.addEventListener('click', () => cmd('setLimits', { [key]: Number(v) }));
    group.appendChild(b);
  }
  for (const b of group.children) b.setAttribute('aria-checked', String(b.dataset.v === v));
}

const history = { ram: [], cpu: [], down: [], up: [] };
let paused = false;
let lastSample = null;

function spark(id, values, colour, max) {
  const c = $(id);
  const g = c.getContext && c.getContext('2d');
  if (!g) return;
  g.clearRect(0, 0, c.width, c.height);
  const top = Math.max(max || 0, ...values, 1);
  g.strokeStyle = colour;
  g.lineWidth = 2;
  g.beginPath();
  // The newest reading at the right edge; a minute of readings across.
  const step = (c.width - 2) / 29;
  values.forEach((v, i) => {
    const x = c.width - 1 - (values.length - 1 - i) * step;
    const y = c.height - 2 - (v / top) * (c.height - 4);
    if (i === 0) g.moveTo(x, y); else g.lineTo(x, y);
  });
  g.stroke();
}

function sampleMonitor() {
  const m = state.monitor;
  if (!m || paused) return;
  const key = JSON.stringify(m);
  if (key === lastSample) return;
  lastSample = key;
  for (const [k, v] of [['ram', m.ramMb], ['cpu', m.cpu], ['down', m.downKbps], ['up', m.upKbps]]) {
    history[k].push(v);
    if (history[k].length > 30) history[k].shift();
  }
}

function renderPanel() {
  if (panel === 'shield') {
    $('p-blocked').textContent = state.shields ? String(state.blocked) : '0';
    $('p-blocked-what').textContent = state.shields ? 'ads and trackers blocked on this page' : 'blocking is off for this site';
    $('p-site').textContent = state.security === 'page' ? 'EON Browser pages are not filtered.' : state.display.split('/')[0];
    const sw = $('p-shields');
    sw.setAttribute('aria-checked', String(state.shields));
    sw.disabled = state.security === 'page';
  } else if (panel === 'route') {
    for (const c of document.querySelectorAll('.choice')) {
      const on = c.dataset.kind === (state.route === 'none' ? 'direct' : state.route) &&
        (c.dataset.address === '' || c.dataset.address === state.routeAddress);
      c.setAttribute('aria-checked', String(on));
      c.tabIndex = 0;
    }
    const own = $('own-address');
    if (document.activeElement !== own && (state.route === 'proxy' ||
        (state.route === 'socks' && !/127\.0\.0\.1:9[01]50$/.test(state.routeAddress || '')))) {
      own.value = state.routeAddress || '';
    }
    const st = $('route-status');
    st.classList.toggle('bad', state.route === 'tor' && state.tor === 'failed');
    st.textContent = state.route === 'tor'
      ? { connecting: 'Connecting to the Tor network. The first time takes a little while.',
          ready: 'Connected to Tor. Each site gets its own circuit.',
          failed: 'Tor could not connect. Your connection may block it.' }[state.tor] || ''
      : state.route === 'socks' || state.route === 'proxy' ? 'Connected through ' + state.routeAddress + '.'
      : state.route === 'none' ? 'The route process is not running; connections go direct.' : '';
    $('new-identity').hidden = !(state.route === 'tor' || state.route === 'socks');
  } else if (panel === 'limits') {
    const l = state.limits;
    choose('sleepAfterMinutes', l.sleepAfterMinutes);
    choose('ramLimitMb', l.ramLimitMb);
    choose('netDownKbps', l.netDownKbps);
    choose('netUpKbps', l.netUpKbps);
  } else if (panel === 'monitor') {
    const m = state.monitor;
    if (!paused) {
      $('m-ram').textContent = memory(m.ramMb);
      $('m-cpu').textContent = m.cpu.toFixed(1) + ' %';
      $('m-down').textContent = speed(m.downKbps);
      $('m-up').textContent = speed(m.upKbps);
      spark('c-ram', history.ram, '#a08bff', state.limits.ramLimitMb);
      spark('c-cpu', history.cpu, '#4fd8e8', 10);
      spark('c-down', history.down, '#74e3ae', 64);
      spark('c-up', history.up, '#ffc861', 64);
    }
    const list = $('m-tabs');
    list.textContent = '';
    state.tabs.forEach((t, i) => {
      const li = el('li');
      li.appendChild(t.asleep ? icon('moon') : el('span', 'dot'));
      li.appendChild(el('span', 'name', t.title || t.url || 'New tab'));
      li.appendChild(el('em', '', i === state.active ? 'in front' : t.asleep ? 'asleep' : 'awake'));
      if (!t.asleep && i !== state.active) {
        const b = el('button', '', 'Sleep');
        b.setAttribute('aria-label', 'Put ' + (t.title || 'this tab') + ' to sleep');
        b.addEventListener('click', () => cmd('sleepTab', { tab: i }));
        li.appendChild(b);
      }
      list.appendChild(li);
    });
  }
}

window.eonState = (s) => {
  state = s;
  sampleMonitor();
  renderTabs();
  renderBar();
  if (panel) renderPanel();
  if (overlayMode === 'palette') { /* the overlay keeps its own list until it closes */ }
};

window.eonFocusAddress = () => {
  const a = $('address');
  a.focus();
  a.select();
};

// ---- suggestions and the command bar, drawn by the overlay ------------------------------

function sendSuggestions() {
  if (!suggestions.length) {
    if (overlayMode === 'suggest') { overlayMode = 'none'; cmd('overlay', { mode: 'none' }); }
    return;
  }
  overlayMode = 'suggest';
  const f = rect($('field'));
  const area = { x: f.x, y: f.y + f.h + 4, w: Math.max(f.w, 320), h: 12 + 36 * suggestions.length };
  cmd('overlay', { mode: 'suggest', area, items: suggestions, selected });
}

function closeSuggestions() {
  if (!suggestions.length && overlayMode !== 'suggest') return;
  suggestions = [];
  selected = -1;
  sendSuggestions();
}

let overlayClosedAt = 0;
window.eonOverlayClosed = () => {
  overlayMode = 'none';
  overlayClosedAt = Date.now();
  suggestions = [];
  selected = -1;
};

let pending = 0;
async function suggest(text) {
  const mine = ++pending;
  const r = await cmd('suggest', { text });
  if (mine !== pending || !editing) return;
  suggestions = (r && r.items) || [];
  selected = -1;
  sendSuggestions();
}

function go(text) {
  editing = false;
  closeSuggestions();
  $('address').blur();
  cmd('go', { text });
}

function commands() {
  const list = [
    { title: 'New tab', hint: 'Ctrl T', run: { op: 'newTab' }, focus: 'chrome' },
    { title: 'New private window', hint: 'Ctrl Shift N', run: { op: 'privateWindow' } },
    { title: 'Reload', hint: 'F5', run: { op: 'reload' } },
    { title: state && state.bookmarked ? 'Remove the bookmark' : 'Bookmark this page', hint: 'Ctrl D', run: { op: 'bookmark' } },
    { title: 'Shield: what was blocked', run: { op: 'panel', name: 'shield' }, focus: 'chrome' },
    { title: 'Connection: Tor and proxy', run: { op: 'panel', name: 'route' }, focus: 'chrome' },
    { title: 'Limiters: memory, network, sleeping tabs', run: { op: 'panel', name: 'limits' }, focus: 'chrome' },
    { title: 'Monitor: memory, processor, network', run: { op: 'panel', name: 'monitor' }, focus: 'chrome' },
    { title: 'Connect through Tor', run: { op: 'setRoute', kind: 'tor', address: '' } },
    { title: 'Connect directly', run: { op: 'setRoute', kind: 'direct', address: '' } },
    { title: 'New identity: new circuits, sites forget you', hint: 'Ctrl Shift U', run: { op: 'newIdentity' } },
    { title: 'Put the other tabs to sleep', run: { op: 'sleepOthers' } },
    { title: 'Turn blocking on or off for this site', run: { op: 'shields' } },
    { title: 'Extensions', run: { op: 'open', page: 'extensions' } },
    { title: 'History', hint: 'Ctrl H', run: { op: 'open', page: 'history' } },
    { title: 'Bookmarks', run: { op: 'open', page: 'bookmarks' } },
    { title: 'Vertical tabs', run: { op: 'panel', name: 'vertical' } },
    { title: 'Tabs on top', run: { op: 'panel', name: 'top' } },
  ];
  if (state) {
    state.tabs.forEach((t, i) => {
      if (i !== state.active) list.push({ title: 'Go to tab: ' + (t.title || t.url || 'New tab'), hint: t.asleep ? 'asleep' : '', run: { op: 'select', tab: i } });
    });
  }
  return list;
}

// The main menu, at the right end of the bar as in Chrome, Edge and Firefox: everything,
// with its shortcut. Drawn by the overlay, above the page.
function menuItems() {
  const vertical = layoutKind() === 'vertical';
  return [
    { title: 'New tab', hint: 'Ctrl+T', run: { op: 'newTab' }, focus: 'chrome' },
    { title: 'New private window', hint: 'Ctrl+Shift+N', run: { op: 'privateWindow' } },
    { sep: true },
    { title: 'History', hint: 'Ctrl+H', run: { op: 'open', page: 'history' } },
    { title: 'Bookmarks', run: { op: 'open', page: 'bookmarks' } },
    { title: 'Extensions', run: { op: 'open', page: 'extensions' } },
    { sep: true },
    { title: 'Shield', hint: state && state.shields ? state.blocked + ' blocked' : '', run: { op: 'panel', name: 'shield' }, focus: 'chrome' },
    { title: 'Connection: Tor and proxy', hint: routeName(), run: { op: 'panel', name: 'route' }, focus: 'chrome' },
    { title: 'Limiters', run: { op: 'panel', name: 'limits' }, focus: 'chrome' },
    { title: 'Monitor', run: { op: 'panel', name: 'monitor' }, focus: 'chrome' },
    { sep: true },
    { title: 'Zoom in', hint: 'Ctrl++', run: { op: 'zoom', to: 'in' } },
    { title: 'Zoom out', hint: 'Ctrl+−', run: { op: 'zoom', to: 'out' } },
    { title: 'Actual size', hint: 'Ctrl+0', run: { op: 'zoom', to: 'reset' } },
    { sep: true },
    { title: vertical ? 'Tabs on top' : 'Vertical tabs', run: { op: 'panel', name: vertical ? 'top' : 'vertical' } },
    { title: 'Command bar', hint: 'Ctrl+K', run: { op: 'palette' } },
    { title: 'About EON Browser', run: { op: 'open', page: 'about' } },
  ];
}

window.eonMenu = () => {
  if (overlayMode === 'menu' || Date.now() - overlayClosedAt < 300) return;
  closeSuggestions();
  const b = $('menu').getBoundingClientRect();
  const items = menuItems();
  const rows = items.filter((i) => !i.sep).length;
  const seps = items.length - rows;
  const w = 300;
  const h = Math.min(12 + rows * 34 + seps * 9, window.innerHeight - b.bottom - 12);
  const area = { x: Math.max(8, b.right - w), y: b.bottom + 4, w, h };
  overlayMode = 'menu';
  cmd('overlay', { mode: 'menu', area, items });
};

window.eonPalette = () => {
  closeSuggestions();
  const p = rect($('page'));
  const w = Math.min(560, Math.max(320, p.w - 32));
  const area = { x: p.x + (p.w - w) / 2, y: p.y + 48, w, h: Math.min(400, Math.max(160, p.h - 64)) };
  overlayMode = 'palette';
  cmd('overlay', { mode: 'palette', area, commands: commands() });
};

// ---- wiring ---------------------------------------------------------------------------

const address = $('address');
address.addEventListener('focus', () => { editing = true; address.select(); if (state) renderBar(); });
address.addEventListener('blur', () => {
  editing = false;
  // A click on a suggestion lands in the overlay after this blur; give it a moment.
  setTimeout(() => { if (!editing) closeSuggestions(); }, 250);
  if (state) renderBar();
});
address.addEventListener('input', () => suggest(address.value));
address.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') {
    e.preventDefault();
    go(selected >= 0 ? suggestions[selected].go : address.value);
  } else if (e.key === 'Escape') {
    e.preventDefault();
    if (suggestions.length) { closeSuggestions(); return; }
    editing = false;
    address.blur();
    if (state) renderBar();
    cmd('focusPage');
  } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    if (!suggestions.length) return;
    e.preventDefault();
    const n = suggestions.length;
    selected = e.key === 'ArrowDown' ? (selected + 1) % n : (selected - 1 + n) % n;
    address.value = suggestions[selected].go;
    sendSuggestions();
  }
});

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && panel && document.activeElement !== address) closePanel();
});

$('back').addEventListener('click', () => cmd('back'));
$('forward').addEventListener('click', () => cmd('forward'));
$('reload').addEventListener('click', () => cmd('reload'));
$('new-tab').addEventListener('click', () => cmd('newTab'));
$('star').addEventListener('click', () => cmd('bookmark'));
$('menu').addEventListener('click', () => window.eonMenu());
$('panel-close').addEventListener('click', closePanel);
for (const b of document.querySelectorAll('[data-panel]')) b.addEventListener('click', () => openPanel(b.dataset.panel));
for (const b of document.querySelectorAll('[data-open]')) b.addEventListener('click', () => cmd('open', { page: b.dataset.open }));

$('p-shields').addEventListener('click', () => cmd('shields'));
for (const c of document.querySelectorAll('.choice')) {
  c.addEventListener('click', () => cmd('setRoute', { kind: c.dataset.kind, address: c.dataset.address }));
}
$('own-proxy').addEventListener('submit', (e) => {
  e.preventDefault();
  const v = $('own-address').value.trim();
  const m = /^(socks5h?|http):\/\/[^\s/]+:\d{1,5}$/.exec(v);
  $('own-error').hidden = !!m;
  if (!m) return;
  cmd('setRoute', { kind: m[1] === 'http' ? 'proxy' : 'socks', address: v });
});
$('new-identity').addEventListener('click', () => cmd('newIdentity'));
for (const g of document.querySelectorAll('.seg')) {
  for (const b of g.children) {
    b.addEventListener('click', () => cmd('setLimits', { [g.dataset.key]: Number(b.dataset.v) }));
  }
}
$('sleep-others').addEventListener('click', () => cmd('sleepOthers'));
$('m-pause').addEventListener('click', () => {
  paused = !paused;
  $('m-pause').textContent = paused ? 'Resume' : 'Pause';
  $('m-pause').setAttribute('aria-pressed', String(paused));
});

setLayout(recall('eon.tabs') === 'vertical' ? 'vertical' : 'top');
cmd('ready').then((s) => { if (s && s.tabs) window.eonState(s); reportLayout(); });
