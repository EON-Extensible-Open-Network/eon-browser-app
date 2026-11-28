// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

// The overlay: suggestions while the address is being typed (the bar keeps the keyboard),
// and the command bar (this view has the keyboard). The browser hands it what to show
// with `eonOverlay`.
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

function icon(name) {
  const img = document.createElement('img');
  img.src = 'icons/' + name + '.svg';
  img.alt = '';
  return img;
}

let mode = 'none';
// The index into `shown` of each row that can be chosen (menus have separators).
const choosable = () => shown.map((s, i) => (s.sep ? -1 : i)).filter((i) => i >= 0);
let all = [];
let shown = [];
let selected = 0;

function done(focus) {
  mode = 'none';
  cmd('closeOverlay', { focusPage: focus !== 'chrome' });
}

async function run(item) {
  // "Command bar" from the menu: the command bar takes the menu's place.
  if (item.run && item.run.op === 'palette') {
    cmd('palette');
    return;
  }
  if (mode === 'suggest') {
    done('page');
    cmd('go', { text: item.go });
    return;
  }
  const focus = item.focus;
  if (item.run) await cmd(item.run.op, item.run);
  else if (item.go) await cmd('go', { text: item.go });
  done(focus);
}

function render() {
  const list = $('list');
  list.textContent = '';
  if (!shown.length) {
    list.appendChild(el('li', 'empty', 'Nothing matches.'));
    return;
  }
  shown.forEach((s, i) => {
    if (s.sep) {
      list.appendChild(el('li', 'sep'));
      return;
    }
    const li = el('li', i === selected ? 'sel' : '');
    li.setAttribute('role', 'option');
    li.setAttribute('aria-selected', String(i === selected));
    if (mode === 'suggest') {
      li.appendChild(icon(s.kind === 'search' ? 'search' : s.kind === 'url' ? 'url' : 'clock'));
      li.appendChild(el('span', 't', s.title));
      li.appendChild(el('span', 'u', s.detail));
    } else {
      if (mode !== 'menu') li.appendChild(icon(s.go ? 'search' : 'arrow'));
      li.appendChild(el('span', 'u t', s.title));
      if (s.hint) li.appendChild(el(mode === 'menu' ? 'span' : 'kbd', mode === 'menu' ? 'hint' : '', s.hint));
      li.setAttribute('role', mode === 'menu' ? 'menuitem' : 'option');
    }
    li.addEventListener('mousedown', (e) => { e.preventDefault(); run(s); });
    list.appendChild(li);
  });
  const sel = list.children[selected];
  if (sel && sel.scrollIntoView) sel.scrollIntoView({ block: 'nearest' });
}

function filter() {
  const q = $('q').value.trim().toLowerCase();
  const words = q.split(/\s+/).filter(Boolean);
  shown = all.filter((c) => words.every((w) => c.title.toLowerCase().includes(w)));
  if (q) shown.push({ title: 'Search or go to “' + $('q').value.trim() + '”', go: $('q').value.trim() });
  selected = 0;
  render();
}

window.eonOverlay = (c) => {
  mode = c.mode;
  document.body.classList.toggle('palette', mode === 'palette');
  document.body.classList.toggle('menu', mode === 'menu');
  $('list').setAttribute('role', mode === 'menu' ? 'menu' : 'listbox');
  if (mode === 'suggest') {
    all = c.items || [];
    shown = all;
    selected = c.selected;
    render();
  } else if (mode === 'palette') {
    all = c.commands || [];
    $('q').value = '';
    filter();
    $('q').focus();
  } else if (mode === 'menu') {
    all = c.items || [];
    shown = all;
    selected = choosable()[0] || 0;
    $('q').blur();
    render();
  }
};

$('q').addEventListener('input', filter);
document.addEventListener('keydown', (e) => {
  if (mode !== 'palette' && mode !== 'menu') return;
  if (e.key === 'Escape') {
    e.preventDefault();
    done('page');
  } else if (e.key === 'Enter') {
    e.preventDefault();
    if (shown[selected]) run(shown[selected]);
  } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    e.preventDefault();
    const rows = choosable();
    const n = rows.length || 1;
    const at = Math.max(0, rows.indexOf(selected));
    selected = rows[e.key === 'ArrowDown' ? (at + 1) % n : (at - 1 + n) % n] || 0;
    render();
  }
});
