/*
 * The page a sign-in hand-over QR code opens on a new device:
 * /family/signin#t=<token>&k=<key>&s=<server>&h=<home>, with `u=<username>` when the app adds
 * it. Everything stays in the part after #, which browsers never send to a server, and this
 * page sends nothing either: it hands the same values to the Octo app, which redeems the token
 * and opens the sealed sign-in with the key.
 *
 * On Android the app opens at once through an intent link that comes back here (with back=1)
 * when Octo is not installed; on a computer the page offers Open in Octo. Without Octo, it shows
 * the server (and username) to type into any other app with the password.
 */
(function () {
  'use strict';

  var F = window.OctoFamily;
  var el = F.el;
  var root = document.getElementById('signin');
  document.getElementById('host').textContent = location.host;

  var fragment = location.hash.replace(/^#/, '');
  var p = new URLSearchParams(fragment);
  var token = p.get('t') || '';
  var key = p.get('k') || '';
  var server = p.get('s') || location.origin;
  var home = p.get('h') || '';
  var username = p.get('u') || '';
  var back = p.has('back');
  // The key should not stay in the address bar or the history once read.
  if (fragment) history.replaceState(null, '', location.pathname);

  function card() {
    var node = el('section', { class: 'info-card fam-card' });
    for (var i = 0; i < arguments.length; i++) F.append(node, arguments[i]);
    return node;
  }

  function heading(text, lede) {
    return el('header', { class: 'page-header fam-join-head' },
      el('div', null, el('h1', { tabindex: '-1' }, text), lede ? el('p', null, lede) : null));
  }

  // What the Octo app takes: the hand-over token, the key, the server and the home address.
  function handOverQuery() {
    var q = new URLSearchParams();
    q.set('t', token);
    q.set('k', key);
    q.set('s', server);
    if (home) q.set('h', home);
    if (username) q.set('u', username);
    return q.toString();
  }

  function openLink() {
    var platform = F.platform();
    if (platform === 'android') {
      var fallback = location.origin + '/family/signin#' + handOverQuery() + '&back=1';
      return F.intentLink('handover', handOverQuery(), fallback);
    }
    if (platform === 'desktop') return 'octo://handover?' + handOverQuery();
    return null;
  }

  function otherApp() {
    var rows = [F.row('Server', server, { copy: 'Copy the server' })];
    if (home && home !== server) rows.push(F.row('At home', home, { copy: 'Copy the home address' }));
    rows.push(username
      ? F.row('Username', username, { copy: 'Copy the username' })
      : F.row('Username', 'Your username', { plain: true }));
    rows.push(F.row('Password', 'The one you chose', { plain: true }));
    return card(
      el('h2', { class: 'info-card-title' }, 'With another app'),
      el('p', { class: 'fam-card-desc' }, 'This code signs in the Octo app only. In Symfonium, Feishin or any Subsonic app, type your login: the server below, your username and your password.'),
      el('div', { class: 'fam-gap fam-pop-stack' },
        F.table.apply(null, rows),
        el('section', { class: 'fam-pop-block' },
          el('h3', { class: 'fam-cap' }, 'Steps for your app'),
          F.appList(null))));
  }

  function render() {
    if (!token || !key) {
      F.clear(root);
      F.append(root, [
        heading('This code is not complete', 'Show a new code on your other device, then scan it again.'),
        card(el('div', { class: 'fam-actions', style: 'margin:0' }, el('a', { class: 'btn btn-primary fam-big', href: '/family/' }, 'Open the Family page')))
      ]);
      return;
    }
    var link = openLink();
    var octo = card(
      el('h2', { class: 'info-card-title' }, 'With the Octo app'),
      el('p', { class: 'fam-card-desc' }, link
        ? 'Press Open in Octo, then allow it on your other device. If nothing happens, Octo is not installed yet: see Get Octo below.'
        : 'Octo has no app for this device yet, so use another app below.'),
      link ? el('div', { class: 'fam-actions' }, el('a', { class: 'btn btn-primary fam-big', href: link }, 'Open in Octo')) : null,
      back ? el('p', { class: 'inline-note info' }, F.icon('info'), 'Octo did not open, so it may not be installed on this phone.') : null,
      el('ol', { class: 'fam-steps fam-signin-steps' },
        el('li', null, 'Octo opens and asks your other device.'),
        el('li', null, 'On your other device, press Allow.'),
        el('li', null, 'This device is signed in, with nothing to type.')));
    F.clear(root);
    F.append(root, [
      heading('Sign in on this device', 'Your other device made this code to sign this one in to ' + hostOf(server) + '.'),
      octo,
      card(el('h2', { class: 'info-card-title' }, 'Get Octo'), el('div', { class: 'fam-gap' }, F.getOcto())),
      otherApp()
    ]);
    var h1 = root.querySelector('h1');
    if (h1) h1.focus({ preventScroll: true });
    // Opens the app at once on Android; without it, Android comes back here with back=1.
    if (F.platform() === 'android' && !back) location.href = link;
  }

  function hostOf(url) {
    try { return new URL(url).host; } catch (e) { return url; }
  }

  render();
})();
