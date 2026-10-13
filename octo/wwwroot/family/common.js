/*
 * What the Family pages share: calls to /api/family, building elements, the dashboard's icons,
 * sheets and dialogs, the toast, copy buttons, labeled rows, sizes and times in words, QR codes,
 * a person's login with the steps for each music app, and a password strength hint.
 */
(function () {
  'use strict';

  var F = (window.OctoFamily = {});

  // ── Calls ────────────────────────────────────────────────────────────────────────────
  // Every write carries X-Octo-Family: 1. Answers { ok, status, data } and never throws.
  F.api = async function (method, path, body) {
    var options = { method: method, credentials: 'same-origin', headers: {} };
    if (method !== 'GET') {
      options.headers['X-Octo-Family'] = '1';
    }
    if (body !== undefined) {
      options.headers['Content-Type'] = 'application/json';
      options.body = JSON.stringify(body);
    }
    try {
      var res = await fetch('/api/family' + path, options);
      var data = null;
      var text = await res.text();
      if (text) {
        try { data = JSON.parse(text); } catch (e) { data = { error: text }; }
      }
      return { ok: res.ok, status: res.status, data: data };
    } catch (e) {
      return { ok: false, status: 0, data: { error: 'Octo did not answer. Check your connection and try again.' } };
    }
  };

  F.errorOf = function (reply) {
    return (reply && reply.data && reply.data.error) || 'Something went wrong. Try again.';
  };

  // ── Elements ─────────────────────────────────────────────────────────────────────────
  // el('div', { class: 'x', onclick: fn }, child, 'text', [more]). Text is always text.
  F.el = function (tag, props) {
    var node = document.createElement(tag);
    if (props) {
      Object.keys(props).forEach(function (key) {
        var value = props[key];
        if (value === null || value === undefined || value === false) return;
        if (key.indexOf('on') === 0 && typeof value === 'function') {
          node.addEventListener(key.slice(2), value);
        } else if (key === 'class') {
          node.className = value;
        } else if (key === 'text') {
          node.textContent = value;
        } else if (key === 'value') {
          node.value = value;
        } else if (key === 'checked') {
          node.checked = !!value;
        } else {
          node.setAttribute(key, value === true ? '' : String(value));
        }
      });
    }
    for (var i = 2; i < arguments.length; i++) F.append(node, arguments[i]);
    return node;
  };

  F.append = function (node, child) {
    if (child === null || child === undefined || child === false) return;
    if (Array.isArray(child)) {
      child.forEach(function (c) { F.append(node, c); });
    } else if (typeof child === 'string' || typeof child === 'number') {
      node.appendChild(document.createTextNode(String(child)));
    } else {
      node.appendChild(child);
    }
  };

  F.clear = function (node) {
    while (node.firstChild) node.removeChild(node.firstChild);
    return node;
  };

  // ── Icons ────────────────────────────────────────────────────────────────────────────
  // The dashboard's sprite (icons.svg, Phosphor) goes in the page itself, as the dashboard
  // does, and F.sprite('i-name') draws one of its symbols.
  var SVG_NS = 'http://www.w3.org/2000/svg';
  fetch('/admin/icons.svg')
    .then(function (r) { return r.ok ? r.text() : ''; })
    .then(function (svg) { if (svg) document.body.insertAdjacentHTML('afterbegin', svg); })
    .catch(function () {});

  F.sprite = function (name, extra) {
    var svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('class', 'icon' + (name.indexOf('b-') === 0 ? ' brand' : '') + (extra ? ' ' + extra : ''));
    svg.setAttribute('aria-hidden', 'true');
    svg.setAttribute('focusable', 'false');
    var use = document.createElementNS(SVG_NS, 'use');
    use.setAttribute('href', '#' + name);
    svg.appendChild(use);
    return svg;
  };

  // Phosphor Icons 2.1.1 (regular), MIT: the glyphs the sprite lacks, drawn inline.
  F.ICONS = {
    link: 'M240,88.23a54.43,54.43,0,0,1-16,37L189.25,160a54.27,54.27,0,0,1-38.63,16h-.05A54.63,54.63,0,0,1,96,119.84a8,8,0,0,1,16,.45A38.62,38.62,0,0,0,150.58,160h0a38.39,38.39,0,0,0,27.31-11.31l34.75-34.75a38.63,38.63,0,0,0-54.63-54.63l-11,11A8,8,0,0,1,135.7,59l11-11A54.65,54.65,0,0,1,224,48,54.86,54.86,0,0,1,240,88.23ZM109,185.66l-11,11A38.41,38.41,0,0,1,70.6,208h0a38.63,38.63,0,0,1-27.29-65.94L78,107.31A38.63,38.63,0,0,1,144,135.71a8,8,0,0,0,16,.45A54.86,54.86,0,0,0,144,96a54.65,54.65,0,0,0-77.27,0L32,130.75A54.62,54.62,0,0,0,70.56,224h0a54.28,54.28,0,0,0,38.64-16l11-11A8,8,0,0,0,109,185.66Z',
    back: 'M165.66,202.34a8,8,0,0,1-11.32,11.32l-80-80a8,8,0,0,1,0-11.32l80-80a8,8,0,0,1,11.32,11.32L91.31,128Z',
    users: 'M117.25,157.92a60,60,0,1,0-66.5,0A95.83,95.83,0,0,0,3.53,195.63a8,8,0,1,0,13.4,8.74,80,80,0,0,1,134.14,0,8,8,0,0,0,13.4-8.74A95.83,95.83,0,0,0,117.25,157.92ZM40,108a44,44,0,1,1,44,44A44.05,44.05,0,0,1,40,108Zm210.14,98.7a8,8,0,0,1-11.07-2.33A79.83,79.83,0,0,0,172,168a8,8,0,0,1,0-16,44,44,0,1,0-16.34-84.87,8,8,0,1,1-5.94-14.85,60,60,0,0,1,55.53,105.64,95.83,95.83,0,0,1,47.22,37.71A8,8,0,0,1,250.14,206.7Z',
    phone: 'M176,16H80A24,24,0,0,0,56,40V216a24,24,0,0,0,24,24h96a24,24,0,0,0,24-24V40A24,24,0,0,0,176,16Zm8,200a8,8,0,0,1-8,8H80a8,8,0,0,1-8-8V40a8,8,0,0,1,8-8h96a8,8,0,0,1,8,8ZM140,60a12,12,0,1,1-12-12A12,12,0,0,1,140,60Z',
    house: 'M219.31,108.68l-80-80a16,16,0,0,0-22.62,0l-80,80A15.87,15.87,0,0,0,32,120v96a8,8,0,0,0,8,8h64a8,8,0,0,0,8-8V160h32v56a8,8,0,0,0,8,8h64a8,8,0,0,0,8-8V120A15.87,15.87,0,0,0,219.31,108.68ZM208,208H160V152a8,8,0,0,0-8-8H104a8,8,0,0,0-8,8v56H48V120l80-80,80,80Z',
    globe: 'M128,24h0A104,104,0,1,0,232,128,104.12,104.12,0,0,0,128,24Zm88,104a87.61,87.61,0,0,1-3.33,24H174.16a157.44,157.44,0,0,0,0-48h38.51A87.61,87.61,0,0,1,216,128ZM102,168H154a115.11,115.11,0,0,1-26,45A115.27,115.27,0,0,1,102,168Zm-3.9-16a140.84,140.84,0,0,1,0-48h59.88a140.84,140.84,0,0,1,0,48ZM40,128a87.61,87.61,0,0,1,3.33-24H81.84a157.44,157.44,0,0,0,0,48H43.33A87.61,87.61,0,0,1,40,128ZM154,88H102a115.11,115.11,0,0,1,26-45A115.27,115.27,0,0,1,154,88Zm52.33,0H170.71a135.28,135.28,0,0,0-22.3-45.6A88.29,88.29,0,0,1,206.37,88ZM107.59,42.4A135.28,135.28,0,0,0,85.29,88H49.63A88.29,88.29,0,0,1,107.59,42.4ZM49.63,168H85.29a135.28,135.28,0,0,0,22.3,45.6A88.29,88.29,0,0,1,49.63,168Zm98.78,45.6a135.28,135.28,0,0,0,22.3-45.6h35.66A88.29,88.29,0,0,1,148.41,213.6Z',
    menu: 'M224,128a8,8,0,0,1-8,8H40a8,8,0,0,1,0-16H216A8,8,0,0,1,224,128ZM40,72H216a8,8,0,0,0,0-16H40a8,8,0,0,0,0,16ZM216,184H40a8,8,0,0,0,0,16H216a8,8,0,0,0,0-16Z',
    qr: 'M104,40H56A16,16,0,0,0,40,56v48a16,16,0,0,0,16,16h48a16,16,0,0,0,16-16V56A16,16,0,0,0,104,40Zm0,64H56V56h48v48Zm0,32H56a16,16,0,0,0-16,16v48a16,16,0,0,0,16,16h48a16,16,0,0,0,16-16V152A16,16,0,0,0,104,136Zm0,64H56V152h48v48ZM200,40H152a16,16,0,0,0-16,16v48a16,16,0,0,0,16,16h48a16,16,0,0,0,16-16V56A16,16,0,0,0,200,40Zm0,64H152V56h48v48Zm-64,72V144a8,8,0,0,1,16,0v32a8,8,0,0,1-16,0Zm80-16a8,8,0,0,1-8,8H184v40a8,8,0,0,1-8,8H144a8,8,0,0,1,0-16h24V144a8,8,0,0,1,16,0v8h24A8,8,0,0,1,216,160Zm0,32v16a8,8,0,0,1-16,0V192a8,8,0,0,1,16,0Z',
    textbox: 'M112,40a8,8,0,0,0-8,8V64H24A16,16,0,0,0,8,80v96a16,16,0,0,0,16,16h80v16a8,8,0,0,0,16,0V48A8,8,0,0,0,112,40ZM24,176V80h80v96ZM248,80v96a16,16,0,0,1-16,16H144a8,8,0,0,1,0-16h88V80H144a8,8,0,0,1,0-16h88A16,16,0,0,1,248,80ZM88,112a8,8,0,0,1-8,8H72v24a8,8,0,0,1-16,0V120H48a8,8,0,0,1,0-16H80A8,8,0,0,1,88,112Z',
    share: 'M227.32,28.68a16,16,0,0,0-15.66-4.08l-.15,0L19.57,82.84a16,16,0,0,0-2.49,29.8L102,154l41.3,84.87A15.86,15.86,0,0,0,157.74,248q.69,0,1.38-.06a15.88,15.88,0,0,0,14-11.51l58.2-191.94c0-.05,0-.1,0-.15A16,16,0,0,0,227.32,28.68ZM157.83,231.85l-.05.14,0-.07-40.06-82.3,48-48a8,8,0,0,0-11.31-11.31l-48,48L24.08,98.25l-.07,0,.14,0L216,40Z'
  };

  // Names from the sprite, or the inline glyphs above.
  var SPRITE = {
    copy: 'i-copy', check: 'i-check', x: 'i-x', caret: 'i-caret-right', retry: 'i-arrow-clockwise',
    warning: 'i-warning', clock: 'i-clock', key: 'i-key', info: 'i-info', plus: 'i-plus', out: 'i-arrow-square-out',
    trash: 'i-trash', heart: 'i-heart', bell: 'i-bell', sliders: 'i-sliders-horizontal', pulse: 'i-pulse',
    playlist: 'i-playlist', tray: 'i-tray-arrow-down', folder: 'i-folder-open', drives: 'i-hard-drives',
    signin: 'i-sign-in', plugs: 'i-plugs-connected', send: 'i-paper-plane-tilt', checkCircle: 'i-check-circle',
    stack: 'i-stack', tag: 'i-tag', radio: 'i-radio', caretDown: 'i-caret-down'
  };

  F.icon = function (name) {
    if (SPRITE[name]) return F.sprite(SPRITE[name]);
    var svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('viewBox', '0 0 256 256');
    svg.setAttribute('class', 'icon');
    svg.setAttribute('aria-hidden', 'true');
    svg.setAttribute('focusable', 'false');
    var path = document.createElementNS(SVG_NS, 'path');
    path.setAttribute('d', F.ICONS[name] || '');
    path.setAttribute('fill', 'currentColor');
    svg.appendChild(path);
    return svg;
  };

  // ── Toast ────────────────────────────────────────────────────────────────────────────
  // The dashboard's toast: news that is not beside a button.
  var toastTimer = null;
  F.toast = function (message, bad) {
    var toast = document.getElementById('toast');
    if (!toast) return;
    toast.textContent = message;
    toast.className = 'show ' + (bad ? 'error' : 'ok');
    clearTimeout(toastTimer);
    toastTimer = setTimeout(function () { toast.className = ''; }, bad ? 8000 : 3500);
    toast.onclick = function () { toast.className = ''; };
  };

  // ── Sheets and dialogs ───────────────────────────────────────────────────────────────
  // Whatever an open dialog runs (its countdown, say) stops when the sheet closes or another
  // sheet takes its place.
  var cleanups = [];
  function runCleanups() {
    var list = cleanups;
    cleanups = [];
    list.forEach(function (fn) { try { fn(); } catch (ignored) { /* keep closing */ } });
  }

  function sheetElement(kind) {
    var sheet = document.getElementById('sheet');
    if (!sheet.hasAttribute('data-watched')) {
      sheet.setAttribute('data-watched', '');
      sheet.addEventListener('close', runCleanups);
      // A click on the backdrop (the dialog itself, outside its frame) closes it.
      sheet.addEventListener('click', function (e) { if (e.target === sheet) F.closeSheet(); });
    }
    runCleanups();
    F.clear(sheet);
    sheet.className = 'fam-sheet ' + (kind || '');
    sheet.removeAttribute('aria-describedby');
    return sheet;
  }

  function closeButton() {
    return F.el('button', { class: 'btn btn-ghost btn-icon fam-close', type: 'button', 'aria-label': 'Close', onclick: F.closeSheet }, F.icon('x'));
  }

  // The scrolling part of a sheet, between its fixed header and footer. When there is more
  // below than shows, a fade and a "More below" button say so, and the button scrolls on;
  // both go once the end is in view. Nothing is ever cut off without a cue.
  function scroller(body) {
    var more = F.el('button', { class: 'fam-more-below', type: 'button', tabindex: '-1', 'aria-hidden': 'true', onclick: function () {
      var calm = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
      body.scrollBy({ top: Math.max(120, body.clientHeight * 0.7), behavior: calm ? 'auto' : 'smooth' });
    } }, 'More below', F.icon('caretDown'));
    var wrap = F.el('div', { class: 'fam-pop-scroll' }, body, F.el('span', { class: 'fam-fade', 'aria-hidden': 'true' }), more);
    var check = function () {
      var rest = body.scrollHeight - body.clientHeight - body.scrollTop;
      wrap.classList.toggle('has-more', rest > 8);
    };
    body.addEventListener('scroll', check, { passive: true });
    var sized = window.ResizeObserver ? new ResizeObserver(check) : null;
    var changed = new MutationObserver(function () { requestAnimationFrame(check); });
    if (sized) sized.observe(body);
    changed.observe(body, { childList: true, subtree: true });
    window.addEventListener('resize', check);
    cleanups.push(function () {
      window.removeEventListener('resize', check);
      if (sized) sized.disconnect();
      changed.disconnect();
    });
    requestAnimationFrame(check);
    return wrap;
  }

  // A plain sheet: a title, a close button and `body`. `options.side` makes it a side sheet.
  F.openSheet = function (title, body, options) {
    options = options || {};
    var sheet = sheetElement(options.side ? 'fam-side' : 'fam-pop');
    var foot = options.foot ? F.el('div', { class: 'fam-pop-foot' }, options.foot) : null;
    sheet.appendChild(F.el('div', { class: 'fam-pop-frame' },
      F.el('span', { class: 'fam-handle', 'aria-hidden': 'true' }),
      F.el('div', { class: 'fam-pop-head' },
        F.el('div', { class: 'fam-pop-titles' },
          F.el('h2', { id: 'sheet-title', class: 'fam-pop-title' }, title),
          options.sub ? F.el('p', { class: 'fam-pop-sub', id: 'sheet-sub' }, options.sub) : null),
        closeButton()),
      scroller(F.el('div', { class: 'fam-pop-body' }, body)),
      foot));
    if (options.sub) sheet.setAttribute('aria-describedby', 'sheet-sub');
    if (!sheet.open) sheet.showModal();
    return sheet;
  };

  // A dialog with a title, a line under it, a body and a footer, each filled by the caller.
  // `options.avatar` puts a person's initial before the title. Answers the parts, plus
  // onClose(fn) and isOpen().
  F.openDialog = function (options) {
    options = options || {};
    var sheet = sheetElement('fam-pop' + (options.wide ? ' wide' : ''));
    sheet.setAttribute('aria-describedby', 'sheet-sub');
    var parts = {
      sheet: sheet,
      title: F.el('h2', { id: 'sheet-title', class: 'fam-pop-title' }),
      sub: F.el('p', { id: 'sheet-sub', class: 'fam-pop-sub' }),
      body: F.el('div', { class: 'fam-pop-body' }),
      foot: F.el('div', { class: 'fam-pop-foot' }),
      announcer: F.el('p', { class: 'visually-hidden fam-announce', 'aria-live': 'polite', 'aria-atomic': 'true' })
    };
    sheet.appendChild(F.el('div', { class: 'fam-pop-frame' },
      F.el('span', { class: 'fam-handle', 'aria-hidden': 'true' }),
      F.el('div', { class: 'fam-pop-head' },
        options.avatar ? F.avatar(options.avatar, 'fam-pop-avatar') : null,
        F.el('div', { class: 'fam-pop-titles' }, parts.title, parts.sub),
        closeButton()),
      scroller(parts.body),
      parts.foot,
      parts.announcer));
    var open = true;
    cleanups.push(function () { open = false; });
    parts.onClose = function (fn) { cleanups.push(fn); };
    parts.isOpen = function () { return open; };
    if (!sheet.open) sheet.showModal();
    return parts;
  };

  F.closeSheet = function () {
    var sheet = document.getElementById('sheet');
    if (sheet && sheet.open) sheet.close();
  };

  // A person's initial on a round tile.
  F.avatar = function (name, extra) {
    return F.el('span', { class: 'fam-avatar' + (extra ? ' ' + extra : ''), 'aria-hidden': 'true' }, F.initials(name));
  };

  // Says `text` to a screen reader, from the dialog `from` sits in, or from the page.
  F.announce = function (from, text) {
    var region = from && from.closest ? (from.closest('.fam-sheet') || document).querySelector('.fam-announce') : null;
    if (!region) {
      region = document.getElementById('announce');
      if (!region) {
        region = F.el('p', { id: 'announce', class: 'visually-hidden fam-announce', 'aria-live': 'polite', 'aria-atomic': 'true' });
        document.body.appendChild(region);
      }
    }
    region.textContent = '';
    setTimeout(function () { region.textContent = text; }, 60);
  };

  // ── Copying ──────────────────────────────────────────────────────────────────────────
  // Copies `text`. A quiet copy leaves the feedback to the button that asked.
  F.copy = async function (text, quiet) {
    try {
      await navigator.clipboard.writeText(text);
    } catch (e) {
      var area = F.el('textarea', { class: 'visually-hidden' });
      area.value = text;
      document.body.appendChild(area);
      area.select();
      try { document.execCommand('copy'); } catch (ignored) { /* nothing more to try */ }
      area.remove();
    }
    if (!quiet) F.toast('Copied');
  };

  // An icon button that copies `text`. Its icon turns to a check with "Copied" for 2 seconds.
  F.copyIcon = function (text, label) {
    var timer = null;
    var glyph = F.el('span', { class: 'fam-copy-glyph' }, F.icon('copy'));
    var button = F.el('button', { class: 'fam-icon-btn fam-copy', type: 'button', 'aria-label': label, title: label },
      glyph, F.el('span', { class: 'fam-copy-done', 'aria-hidden': 'true' }, 'Copied'));
    button.addEventListener('click', async function () {
      if (button.disabled) return;
      await F.copy(text, true);
      button.classList.add('copied');
      F.clear(glyph).appendChild(F.icon('check'));
      F.announce(button, 'Copied');
      clearTimeout(timer);
      timer = setTimeout(function () {
        button.classList.remove('copied');
        F.clear(glyph).appendChild(F.icon('copy'));
      }, 2000);
    });
    return button;
  };

  F.copyButton = function (text, label) {
    return F.el('button', { class: 'btn btn-sm', type: 'button', onclick: function () { F.copy(text); } }, F.icon('copy'), label || 'Copy');
  };

  // ── Labeled rows ─────────────────────────────────────────────────────────────────────
  // One row of a quiet table: a label on the left, the value on the right, and a copy button
  // when `copy` names one. `options.tag` sits beside the label; `options.value` draws the value
  // instead of plain text.
  F.row = function (label, text, options) {
    options = options || {};
    return F.el('div', { class: 'fam-trow' + (options.big ? ' big' : '') + (options.key ? ' key' : '') },
      F.el('dt', { class: 'fam-tlabel' }, F.el('span', null, label), options.tag || null),
      F.el('dd', { class: 'fam-tvalue' },
        F.el('span', { class: 'fam-tval' + (options.plain ? '' : ' fam-mono') }, options.value || text),
        options.copy ? F.copyIcon(text, options.copy) : null));
  };

  F.table = function () {
    var table = F.el('dl', { class: 'fam-table' });
    for (var i = 0; i < arguments.length; i++) F.append(table, arguments[i]);
    return table;
  };

  // A labeled value with a copy button, on its own line.
  F.secret = function (label, value) {
    return F.table(F.row(label, value, { copy: 'Copy ' + label.toLowerCase() }));
  };

  // A link on one line: an icon, the link itself (cut in the middle when it is long, whole in
  // its tooltip and in what is copied) and a copy button. `share` adds the phone's share sheet.
  F.linkRow = function (link, copyLabel, share) {
    var tail = Math.min(14, Math.floor(link.length / 3));
    var anchor = F.el('a', { class: 'fam-linkrow-a', href: link, title: link, target: '_blank', rel: 'noopener' },
      F.el('span', { class: 'fam-linkrow-head' }, link.slice(0, link.length - tail)),
      F.el('span', { class: 'fam-linkrow-tail' }, link.slice(link.length - tail)));
    var shareButton = share && navigator.share ? F.el('button', { class: 'fam-icon-btn', type: 'button', 'aria-label': 'Share the link', title: 'Share the link', onclick: function () {
      navigator.share({ title: share.title, text: share.text, url: link }).catch(function () {});
    } }, F.icon('share')) : null;
    return F.el('div', { class: 'fam-linkrow' },
      F.el('span', { class: 'fam-linkrow-ico' }, F.icon('link')),
      anchor,
      shareButton,
      F.copyIcon(link, copyLabel || 'Copy the link'));
  };

  // ── Words ────────────────────────────────────────────────────────────────────────────
  F.bytes = function (n) {
    n = Number(n) || 0;
    if (n === 0) return '0 GB';
    if (n >= 1e9) return (n / 1e9).toFixed(n >= 1e10 ? 0 : 1) + ' GB';
    if (n >= 1e6) return Math.round(n / 1e6) + ' MB';
    if (n >= 1e3) return Math.round(n / 1e3) + ' KB';
    return n + ' bytes';
  };

  F.ago = function (iso) {
    if (!iso) return 'Not seen yet';
    var then = new Date(iso).getTime();
    if (isNaN(then)) return '';
    var seconds = Math.max(0, Math.round((Date.now() - then) / 1000));
    if (seconds < 60) return 'Just now';
    var minutes = Math.round(seconds / 60);
    if (minutes < 60) return minutes === 1 ? 'A minute ago' : minutes + ' minutes ago';
    var hours = Math.round(minutes / 60);
    if (hours < 24) return hours === 1 ? 'An hour ago' : hours + ' hours ago';
    var days = Math.round(hours / 24);
    if (days === 1) return 'Yesterday';
    if (days < 30) return days + ' days ago';
    return new Date(iso).toLocaleDateString();
  };

  F.initials = function (name) {
    var parts = String(name || '?').trim().split(/\s+/);
    return (parts[0].charAt(0) + (parts.length > 1 ? parts[parts.length - 1].charAt(0) : '')).toUpperCase();
  };

  // ── Links ────────────────────────────────────────────────────────────────────────────
  F.inviteLink = function (server, token) {
    return server + '/family/join#invite=' + encodeURIComponent(token);
  };

  // Opens the Octo app at its sign-in screen with the server, home address and username filled
  // in: `octo://signin?server=..&home=..&username=..`.
  F.signInLink = function (server, username, home) {
    return 'octo://signin?server=' + encodeURIComponent(server) +
      (home && home !== server ? '&home=' + encodeURIComponent(home) : '') +
      '&username=' + encodeURIComponent(username);
  };

  // `octo://<path>?<query>` as an Android intent link that comes back to `fallback` when the app
  // is not installed.
  F.intentLink = function (path, query, fallback) {
    return 'intent://' + path + '?' + query + '#Intent;scheme=octo;package=app.winters.octo;S.browser_fallback_url=' +
      encodeURIComponent(fallback) + ';end';
  };

  F.platform = function () {
    var ua = navigator.userAgent || '';
    if (/android/i.test(ua)) return 'android';
    if (/iphone|ipad|ipod/i.test(ua) || (/macintosh/i.test(ua) && navigator.maxTouchPoints > 1)) return 'ios';
    return 'desktop';
  };

  // A QR code of `link` on a white tile.
  F.qrCard = function (link, label) {
    var tile = F.el('div', { class: 'fam-qr-tile' });
    try {
      tile.appendChild(window.OctoQr.svg(link, label || 'QR code for the join link'));
    } catch (e) {
      tile.appendChild(F.el('p', { class: 'fam-small' }, 'This link is too long for a QR code.'));
    }
    return tile;
  };

  // ── A person's login ─────────────────────────────────────────────────────────────────
  // One login works in every app: the server address, the username and the password the person
  // chose. These are the steps for the apps people use most.
  F.APPS = ['Octo', 'Symfonium', 'Feishin', 'Amperfy', 'Substreamer', 'Tempo'];

  F.appSteps = function (app) {
    switch (app) {
      case 'Octo':
        return [
          'Open Octo and choose Sign in.',
          'Type the server, your username and your password.',
          'Press Sign in.'
        ];
      case 'Symfonium':
        return [
          'Open Symfonium and choose Add a media provider, then Subsonic.',
          'Type the server, your username and your password.',
          'Press Test, then Save.'
        ];
      case 'Feishin':
        return [
          'Open Feishin and choose Add server, with Subsonic as the server type.',
          'Type a name, then the server, your username and your password.',
          'Press Add.'
        ];
      case 'Amperfy':
        return [
          'Open Amperfy and type the server on the sign-in screen.',
          'Type your username and your password.',
          'Choose Subsonic if it asks which kind of server this is, then sign in.'
        ];
      case 'Substreamer':
        return [
          'Open Substreamer and choose Add server.',
          'Type the server, your username and your password.',
          'Save, then wait while it reads the library.'
        ];
      default:
        return [
          'Open Tempo and choose Add server on the sign-in screen.',
          'Type a name, then the server, your username and your password.',
          'Save and sign in.'
        ];
    }
  };

  // The steps for each app, behind its name. On Android, Octo's steps also offer to open the
  // app with the server and username filled in.
  F.appList = function (login) {
    var apps = F.el('div', { class: 'fam-applist' });
    var open = login && F.platform() === 'android'
      ? F.el('a', { class: 'btn btn-sm btn-primary', href: F.signInLink(F.loginServer(login), login.username, login.servers.home) }, 'Open in Octo')
      : null;
    F.APPS.forEach(function (app) {
      var steps = F.el('ol', { class: 'fam-steps' });
      F.appSteps(app).forEach(function (line) { steps.appendChild(F.el('li', null, line)); });
      apps.appendChild(F.el('details', { class: 'fam-app', name: 'fam-app-steps' },
        F.el('summary', null, F.el('span', null, app), F.icon('caret')),
        steps,
        app === 'Octo' && open ? F.el('div', { class: 'fam-actions' }, open) : null));
    });
    return apps;
  };

  // The address apps sign in to: the outside address when this person may listen away and the
  // server has one, else the home address.
  F.loginServer = function (login) {
    var servers = login.servers || {};
    return (login.awayAllowed !== false && login.anywhereAvailable && servers.anywhere) || servers.home || location.origin;
  };

  // "Your login": the server (with the home address under it when it differs), the username
  // and the password, each server and the username with a copy button, then the steps for
  // each app. `options.changePassword` puts a Change password button by the password;
  // `options.passwordWords` says what the password is.
  F.loginCard = function (login, options) {
    options = options || {};
    var servers = login.servers || {};
    var server = F.loginServer(login);
    var home = servers.home && servers.home !== server ? servers.home : null;
    var away = login.awayAllowed !== false;
    var serverValue = F.el('span', { class: 'fam-login-servers' },
      F.el('span', { class: 'fam-login-line' }, F.el('span', { class: 'fam-mono' }, server), F.copyIcon(server, 'Copy the server')),
      home ? F.el('span', { class: 'fam-login-line fam-login-home' },
        F.el('span', null, 'At home: ', F.el('span', { class: 'fam-mono' }, home)), F.copyIcon(home, 'Copy the home address')) : null);
    var note = null;
    if (!away) {
      note = F.el('p', { class: 'fam-where-uses' }, F.icon('house'), F.el('span', null, 'Your plan keeps listening to your home network, so this address works at home only.'));
    } else if (!login.anywhereAvailable) {
      note = F.el('p', { class: 'fam-where-uses' }, F.icon('house'), F.el('span', null, 'This address works on your home network. Away from home needs an outside address, which whoever runs this server sets.'));
    }
    var passwordValue = F.el('span', { class: 'fam-login-line' },
      F.el('span', null, options.passwordWords || 'The one you chose'),
      options.changePassword ? F.el('button', { class: 'btn btn-sm btn-ghost', type: 'button', onclick: options.changePassword }, 'Change password') : null);
    return F.el('div', { class: 'fam-login' },
      F.el('dl', { class: 'fam-table' },
        F.el('div', { class: 'fam-trow' }, F.el('dt', { class: 'fam-tlabel' }, F.el('span', null, 'Server')), F.el('dd', { class: 'fam-tvalue' }, serverValue)),
        F.row('Username', login.username, { copy: 'Copy the username' }),
        F.el('div', { class: 'fam-trow' }, F.el('dt', { class: 'fam-tlabel' }, F.el('span', null, 'Password')), F.el('dd', { class: 'fam-tvalue' }, passwordValue))),
      note,
      F.el('section', { class: 'fam-pop-block' },
        F.el('h3', { class: 'fam-cap' }, 'Use this in any app'),
        F.appList(login)));
  };

  // ── Passwords ────────────────────────────────────────────────────────────────────────
  // A plain-words hint on how strong a new password is, under `input`.
  F.strengthHint = function (input) {
    var hint = F.el('span', { class: 'fam-help fam-strength', 'aria-live': 'polite' }, 'At least 8 characters.');
    var update = function () {
      var value = input.value;
      var kinds = [/[a-z]/, /[A-Z]/, /[0-9]/, /[^A-Za-z0-9]/].filter(function (re) { return re.test(value); }).length;
      var words = value.trim().split(/\s+/).filter(Boolean).length;
      var level;
      if (!value) level = ['', 'At least 8 characters.'];
      else if (value.length < 8) level = ['weak', 'Too short: use at least 8 characters.'];
      else if (value.length >= 16 || words >= 4 || (value.length >= 12 && kinds >= 3)) level = ['strong', 'Strong.'];
      else if (value.length >= 12 || kinds >= 3) level = ['good', 'Good. A few more characters make it stronger.'];
      else level = ['fair', 'Fair. Longer is stronger: try a short phrase.'];
      hint.className = 'fam-help fam-strength' + (level[0] ? ' ' + level[0] : '');
      hint.textContent = level[1];
    };
    input.addEventListener('input', update);
    return hint;
  };

  // Where to get Octo, for this device.
  F.FDROID_REPO = 'https://winters27.github.io/octo/fdroid/repo';
  F.RELEASES = 'https://github.com/winters27/octo/releases';

  F.getOcto = function () {
    var platform = F.platform();
    var android = F.el('div', { class: 'fam-get' },
      F.el('h3', { class: 'fam-get-title' }, 'Octo for Android'),
      F.el('ol', { class: 'fam-steps' },
        F.el('li', null, 'In F-Droid, open Settings, then Repositories, and add the address below.'),
        F.el('li', null, 'Search for Octo and install it.')),
      F.secret('F-Droid repository', F.FDROID_REPO),
      F.el('p', { class: 'fam-help' }, 'No F-Droid? ', F.el('a', { href: F.RELEASES, target: '_blank', rel: 'noopener' }, 'Download the app from GitHub'), ' and allow your browser to install it.'));
    var desktop = F.el('div', { class: 'fam-get' },
      F.el('h3', { class: 'fam-get-title' }, 'Octo for Windows, macOS and Linux'),
      F.el('p', { class: 'fam-help' }, 'Download the installer for your computer, open it, then come back to this page.'),
      F.el('div', null, F.el('a', { class: 'btn btn-primary fam-big', href: F.RELEASES, target: '_blank', rel: 'noopener' }, F.icon('tray'), 'Download Octo')));
    if (platform === 'android') return F.el('div', { class: 'fam-stack' }, android, desktop);
    if (platform === 'ios') {
      return F.el('div', { class: 'fam-stack' },
        F.el('p', { class: 'fam-help' }, 'Octo has no iPhone download yet, so use another app on this phone. Amperfy and Substreamer both work.'),
        desktop, android);
    }
    return F.el('div', { class: 'fam-stack' }, desktop, android);
  };
})();
