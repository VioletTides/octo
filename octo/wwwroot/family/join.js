/*
 * The page an invite link or QR code opens: /family/join#invite=<token> sets up a person's login
 * (or a new password after a reset). The token stays in the part after #, which browsers never
 * send to a server.
 *
 * Three steps, one card each: 1 sign up (your name and a password), 2 your login (the server,
 * username and password to use in any app, with each app's steps), 3 done.
 */
(function () {
  'use strict';

  var F = window.OctoFamily;
  var el = F.el;
  var root = document.getElementById('join');
  document.getElementById('host').textContent = location.host;

  var STEPS = ['Sign up', 'Your login', 'Done'];

  function params() {
    return new URLSearchParams(location.hash.replace(/^#/, ''));
  }

  function card() {
    var node = el('section', { class: 'info-card fam-card' });
    for (var i = 0; i < arguments.length; i++) F.append(node, arguments[i]);
    return node;
  }

  function heading(text, lede) {
    return el('header', { class: 'page-header fam-join-head' },
      el('div', null, el('h1', { tabindex: '-1' }, text), lede ? el('p', null, lede) : null));
  }

  function progress(step) {
    return el('ol', { class: 'fam-progress', 'aria-label': 'Step ' + step + ' of ' + STEPS.length },
      STEPS.map(function (words, i) {
        var n = i + 1;
        return el('li', { class: (n <= step ? 'on' : '') + (n === step ? ' now' : ''), 'aria-current': n === step ? 'step' : null },
          el('span', null, n + ' ' + words));
      }));
  }

  function show(parts) {
    F.clear(root);
    F.append(root, parts);
    window.scrollTo(0, 0);
    var h1 = root.querySelector('h1');
    if (h1) h1.focus({ preventScroll: true });
  }

  // ── Step 1: your name and password ───────────────────────────────────────────────────
  function renderInvite(token) {
    var name = el('input', { class: 'set-input', id: 'name', autocomplete: 'name', placeholder: 'Alex' });
    var pass = el('input', { class: 'set-input', id: 'password', type: 'password', autocomplete: 'new-password', minlength: 8, required: true });
    var again = el('input', { class: 'set-input', id: 'password-again', type: 'password', autocomplete: 'new-password', minlength: 8, required: true });
    var error = el('p', { class: 'field-error', role: 'alert' });
    var button = el('button', { class: 'btn btn-primary btn-block fam-big', type: 'submit' }, 'Continue');
    var form = el('form', { class: 'fam-form', onsubmit: async function (e) {
      e.preventDefault();
      error.textContent = '';
      if (pass.value !== again.value) {
        error.textContent = 'The two passwords are not the same.';
        again.focus();
        return;
      }
      button.disabled = true;
      var reply = await F.api('POST', '/join', { token: token, password: pass.value, displayName: name.value.trim() || null });
      button.disabled = false;
      if (!reply.ok) {
        error.textContent = F.errorOf(reply);
        return;
      }
      // The link is used up; the address bar should not keep it.
      history.replaceState(null, '', '/family/join#joined');
      renderLogin(name.value.trim() || reply.data.username);
    } },
    el('label', { class: 'fam-label', for: 'name' }, 'Your name', name, el('span', { class: 'fam-help' }, 'How the family sees you.')),
    el('label', { class: 'fam-label', for: 'password' }, 'Choose a password', pass, F.strengthHint(pass)),
    el('label', { class: 'fam-label', for: 'password-again' }, 'Type it again', again),
    el('p', { class: 'fam-help' }, 'Your username and this password are your login for Octo and any other music app.'),
    error,
    button);
    show([
      progress(1),
      heading('Sign up', 'Someone invited you to listen with them on Octo. Choose your name and a password: with your username, it is your login for every app.'),
      card(form)
    ]);
  }

  // ── Step 2: your login ───────────────────────────────────────────────────────────────
  async function renderLogin(name) {
    show([progress(2), heading('Your login', 'Loading')]);
    var reply = await F.api('GET', '/me/login');
    if (!reply.ok) {
      show([
        progress(2),
        heading('You are in, ' + name, 'Your login is on your Family page.'),
        card(el('div', { class: 'fam-actions', style: 'margin:0' }, el('a', { class: 'btn btn-primary fam-big', href: '/family/#/account' }, 'Open my Family page')))
      ]);
      return;
    }
    var done = el('button', { class: 'btn btn-primary fam-big', type: 'button', onclick: function () { renderDone(name); } }, 'I’m signed in');
    show([
      progress(2),
      heading('Your login', 'You are in, ' + name + '. Use this login in Octo or any Subsonic app, on as many devices as you like.'),
      card(
        el('h2', { class: 'info-card-title' }, 'Your login'),
        el('div', { class: 'fam-gap' }, F.loginCard(reply.data, { passwordWords: 'The one you just chose' }))),
      card(el('h2', { class: 'info-card-title' }, 'Get Octo'), el('div', { class: 'fam-gap' }, F.getOcto())),
      el('div', { class: 'fam-actions' }, done),
      el('p', { class: 'fam-help fam-gap' }, 'Your login, devices and audio quality are always on ', el('a', { href: '/family/#/account' }, 'your Family page'), '.')
    ]);
  }

  // ── Step 3: done ─────────────────────────────────────────────────────────────────────
  function renderDone(name) {
    show([
      progress(3),
      heading('You are all set', 'Welcome, ' + name + '. Your music is ready on every app you sign in to.'),
      card(
        el('div', { class: 'fam-empty' },
          el('span', { class: 'fam-empty-ico', 'aria-hidden': 'true' }, F.icon('check')),
          el('p', null, 'Heart songs in your app to save them. Your login, devices, plan and audio quality are on your Family page.'),
          el('a', { class: 'btn btn-primary fam-big', href: '/family/' }, 'Open my Family page')))
    ]);
  }

  // ── Start ────────────────────────────────────────────────────────────────────────────
  function start() {
    var p = params();
    var invite = p.get('invite');
    if (invite) return renderInvite(invite);
    if (p.has('joined')) {
      F.api('GET', '/session').then(function (reply) {
        if (reply.ok) renderLogin(reply.data.displayName || reply.data.username);
        else location.replace('/family/');
      });
      return;
    }
    if (p.get('c')) {
      show([
        heading('This link no longer works', 'Family codes are gone: everyone has one login now. Sign in on any app with the server, your username and your password.'),
        card(el('div', { class: 'fam-actions', style: 'margin:0' }, el('a', { class: 'btn btn-primary fam-big', href: '/family/' }, 'Open the Family page')))
      ]);
      return;
    }
    show([
      heading('This link is not complete', 'Ask for a new link, or sign in on the Family page.'),
      card(el('div', { class: 'fam-actions', style: 'margin:0' }, el('a', { class: 'btn btn-primary fam-big', href: '/family/' }, 'Open the Family page')))
    ]);
  }

  window.addEventListener('hashchange', start);
  start();
})();
