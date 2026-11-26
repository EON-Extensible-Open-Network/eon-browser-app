// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

// EON Browser's bar. Everything it does goes through `cmd`, a request to a host that
// never resolves; the browser answers it. The browser pushes state with `eonState`.
'use strict';

const $ = (id) => document.getElementById(id);
const STRIP = 38, BAR = 46;

async function cmd(op, args = {}) {
  try {
    const r = await fetch('https://ui.eon.invalid/cmd?c=' +
      encodeURIComponent(JSON.stringify(Object.assign({ op }, args))));
    return await r.json();
  } catch (_) {
    return null;
  }
}

let state = null;
let editing = false;
let suggestions = [];
let selected = -1;

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function icon(name) {
  const img = document.createElement('img');
  img.src = 'icons/' + name + '.svg';
  img.alt = '';
  return img;
}

function renderTabs() {
  const box = $('tabs');
  box.textContent = '';
  state.tabs.forEach((t, i) => {
    const tab = el('div', 'tab' + (i === state.active ? ' active' : '') + (t.loading ? ' loading' : ''));
    tab.setAttribute('role', 'tab');
    tab.title = t.title || t.url;
    tab.appendChild(el('span', 'dot'));
    tab.appendChild(el('span', 'title', t.title || t.url || 'New tab'));
    const x = el('button', 'x');
    x.title = 'Close tab (Ctrl+W)';
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
    ? state.blocked + ' ads and trackers blocked on this page. Click to turn blocking off for this site.'
    : 'Blocking is off for this site. Click to turn it on.';
  $('private').hidden = !state.private;
}

window.eonState = (s) => {
  state = s;
  renderTabs();
  renderBar();
};

window.eonFocusAddress = () => {
  const a = $('address');
  a.focus();
  a.select();
};

function setHeight() {
  const open = !$('suggest').hidden;
  const h = STRIP + BAR + (open ? 12 + 36 * suggestions.length : 0);
  cmd('height', { px: h });
}

function renderSuggestions() {
  const list = $('suggest');
  list.textContent = '';
  suggestions.forEach((s, i) => {
    const li = el('li', i === selected ? 'sel' : '');
    li.appendChild(icon(s.kind === 'search' ? 'search' : s.kind === 'url' ? 'url' : 'clock'));
    li.appendChild(el('span', 't', s.title));
    li.appendChild(el('span', 'u', s.detail));
    li.addEventListener('mousedown', (e) => { e.preventDefault(); go(s.go); });
    list.appendChild(li);
  });
  list.hidden = suggestions.length === 0;
  setHeight();
}

function closeSuggestions() {
  if (suggestions.length === 0 && $('suggest').hidden) return;
  suggestions = [];
  selected = -1;
  renderSuggestions();
}

let pending = 0;
async function suggest(text) {
  const mine = ++pending;
  const r = await cmd('suggest', { text });
  if (mine !== pending || !editing) return;
  suggestions = (r && r.items) || [];
  selected = -1;
  renderSuggestions();
}

function go(text) {
  editing = false;
  closeSuggestions();
  $('address').blur();
  cmd('go', { text });
}

const address = $('address');
address.addEventListener('focus', () => { editing = true; address.select(); renderBar(); });
address.addEventListener('blur', () => { editing = false; closeSuggestions(); if (state) renderBar(); });
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
    renderBar();
    cmd('focusPage');
  } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    if (!suggestions.length) return;
    e.preventDefault();
    const n = suggestions.length;
    selected = e.key === 'ArrowDown' ? (selected + 1) % n : (selected - 1 + n) % n;
    address.value = suggestions[selected].go;
    renderSuggestions();
  }
});

$('back').addEventListener('click', () => cmd('back'));
$('forward').addEventListener('click', () => cmd('forward'));
$('reload').addEventListener('click', () => cmd('reload'));
$('new-tab').addEventListener('click', () => cmd('newTab'));
$('shield').addEventListener('click', () => cmd('shields'));

cmd('ready').then((s) => { if (s && s.tabs) window.eonState(s); });
