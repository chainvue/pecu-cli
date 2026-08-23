/* pecu-cli site — vanilla, no dependencies, no network beyond one lazy JSON. */
(function () {
  'use strict';

  var root = document.documentElement;
  var rel = (document.querySelector('link[rel=stylesheet]').getAttribute('href') || '')
              .replace(/assets\/site\.css$/, '');

  /* ------------------------------------------------------------ theme */

  function currentTheme() {
    if (root.dataset.theme) return root.dataset.theme;
    return matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  }
  var toggle = document.querySelector('[data-theme-toggle]');
  if (toggle) {
    toggle.addEventListener('click', function () {
      var next = currentTheme() === 'dark' ? 'light' : 'dark';
      root.dataset.theme = next;
      try { localStorage.setItem('pecu-theme', next); } catch (e) {}
      toggle.setAttribute('aria-label', 'Switch to ' + (next === 'dark' ? 'light' : 'dark') + ' theme');
    });
  }

  /* ------------------------------------------------------------ copy */

  function copyButton(host, getText, className) {
    var button = document.createElement('button');
    button.type = 'button';
    button.className = className;
    button.textContent = 'Copy';
    button.setAttribute('aria-label', 'Copy to clipboard');
    button.addEventListener('click', function () {
      var text = getText();
      var done = function (ok) {
        button.textContent = ok ? 'Copied' : 'Press ⌘C';
        if (ok) button.dataset.done = '';
        setTimeout(function () { button.textContent = 'Copy'; delete button.dataset.done; }, 1600);
      };
      if (navigator.clipboard && window.isSecureContext) {
        navigator.clipboard.writeText(text).then(function () { done(true); }, function () { done(false); });
      } else {
        // file:// and plain http have no clipboard API. Select it instead, so
        // the reader still gets one keystroke rather than a dead button.
        var area = document.createElement('textarea');
        area.value = text; area.style.position = 'fixed'; area.style.opacity = '0';
        document.body.appendChild(area); area.select();
        var ok = false;
        try { ok = document.execCommand('copy'); } catch (e) {}
        document.body.removeChild(area); done(ok);
      }
    });
    host.appendChild(button);
  }

  document.querySelectorAll('pre:not(.hero-logo)').forEach(function (pre) {
    copyButton(pre, function () { return pre.textContent.replace(/\n$/, ''); }, 'copy');
  });
  document.querySelectorAll('[data-snippet]').forEach(function (snippet) {
    copyButton(snippet, function () { return snippet.dataset.snippet; }, '');
  });

  /* ------------------------------------------------------------ sideways scroll */

  var scrollers = document.querySelectorAll('.table-wrap, pre:not(.hero-logo)');
  scrollers.forEach(function (box) {
    var mark = function () {
      var over = box.scrollWidth - box.clientWidth;
      box.classList.toggle('scrolls', over > 2);
      if (box.scrollLeft >= over - 2) box.dataset.end = '';
      else delete box.dataset.end;
    };
    mark();
    box.addEventListener('scroll', mark, { passive: true });
    if ('ResizeObserver' in window) new ResizeObserver(mark).observe(box);
  });

  /* ------------------------------------------------------------ terminal captures */

  var terms = Array.prototype.slice.call(document.querySelectorAll('[data-term]'));
  if (terms.length) {
    var reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
    // On a narrow screen a capture is scaled small enough that the typing is
    // not readable anyway, and playing it means someone scrolling past sees an
    // empty terminal for five seconds instead of the output the section is
    // talking about. The finished frame is the informative one, so that is what
    // a phone gets; Replay is still there for anyone who wants the animation.
    var narrow = matchMedia('(max-width: 46rem)').matches;
    terms.forEach(function (term) {
      var replay = term.querySelector('[data-replay]');
      if (replay && !reduced) {
        replay.hidden = false;
        replay.addEventListener('click', function () {
          // Restarting a CSS animation needs the class off, a reflow, then on.
          term.classList.remove('is-playing');
          void term.offsetWidth;
          term.classList.add('is-playing');
        });
      }
    });
    // The head already decided; agreeing with it keeps one rule in one place.
    var deferred = root.classList.contains('term-defer');
    if (deferred) {
      var seen = new IntersectionObserver(function (entries) {
        entries.forEach(function (entry) {
          if (!entry.isIntersecting) return;
          entry.target.classList.add('is-playing');
          seen.unobserve(entry.target);
        });
      }, { threshold: 0.25 });
      terms.forEach(function (term) { seen.observe(term); });
    }
    // Nothing to do in the other branch: without `term-defer` the capture is
    // already resting on its finished frame, which is the state worth showing.
  }

  /* ------------------------------------------------------------ scroll spy */

  var tocLinks = Array.prototype.slice.call(document.querySelectorAll('.sidebar-toc a'));
  if (tocLinks.length && 'IntersectionObserver' in window) {
    var byId = {};
    tocLinks.forEach(function (a) { byId[a.getAttribute('href').slice(1)] = a; });
    var visible = new Set();
    var spy = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) visible.add(entry.target.id);
        else visible.delete(entry.target.id);
      });
      var first = tocLinks.map(function (a) { return a.getAttribute('href').slice(1); })
                          .find(function (id) { return visible.has(id); });
      tocLinks.forEach(function (a) { a.classList.remove('active'); });
      if (first && byId[first]) {
        byId[first].classList.add('active');
        var box = byId[first].getBoundingClientRect();
        var pane = document.querySelector('.sidebar');
        if (pane && (box.top < 80 || box.bottom > window.innerHeight - 40)) {
          byId[first].scrollIntoView({ block: 'nearest' });
        }
      }
    }, { rootMargin: '-72px 0px -70% 0px' });
    Object.keys(byId).forEach(function (id) {
      var target = document.getElementById(id);
      if (target) spy.observe(target);
    });
  }

  /* ------------------------------------------------------------ search */

  var palette = document.querySelector('[data-palette]');
  if (!palette) return;
  var input = palette.querySelector('[data-palette-input]');
  var list = palette.querySelector('[data-palette-results]');
  var records = null;
  var loading = null;
  var cursor = 0;
  var hits = [];
  var restoreFocus = null;

  function load() {
    if (records) return Promise.resolve(records);
    if (!loading) {
      loading = fetch(rel + 'assets/search.json')
        .then(function (r) { return r.json(); })
        .then(function (data) { records = data; return data; })
        .catch(function () { records = []; return []; });
    }
    return loading;
  }

  function open() {
    if (!palette.hidden) return;
    restoreFocus = document.activeElement;
    palette.hidden = false;
    input.value = '';
    render([]);
    input.focus();
    load().then(function () { if (!palette.hidden) search(input.value); });
  }

  function close() {
    palette.hidden = true;
    if (restoreFocus && restoreFocus.focus) restoreFocus.focus();
  }

  function escapeHtml(text) {
    return text.replace(/[&<>"]/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c];
    });
  }

  function score(record, terms) {
    var title = record.title.toLowerCase();
    var text = record.text.toLowerCase();
    var total = 0;
    for (var i = 0; i < terms.length; i++) {
      var t = terms[i];
      var inTitle = title.indexOf(t);
      var inText = text.indexOf(t);
      if (inTitle < 0 && inText < 0) return 0;           // every term must appear
      if (inTitle === 0) total += 60;
      else if (inTitle > 0) total += 34;
      if (inText >= 0) total += 8;
    }
    if (title === terms.join(' ')) total += 80;
    if (!record.anchor) total += 4;                       // page intros rank a little higher
    return total;
  }

  function excerpt(record, terms) {
    var text = record.text;
    var lower = text.toLowerCase();
    var at = -1;
    for (var i = 0; i < terms.length && at < 0; i++) at = lower.indexOf(terms[i]);
    var start = at < 0 ? 0 : Math.max(0, at - 40);
    var slice = text.slice(start, start + 170);
    var out = escapeHtml((start > 0 ? '…' : '') + slice);
    terms.forEach(function (t) {
      out = out.replace(new RegExp('(' + t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + ')', 'ig'),
                        '<mark>$1</mark>');
    });
    return out;
  }

  function href(record) {
    return rel + record.slug + '/' + (record.anchor ? '#' + record.anchor : '');
  }

  function render(results, terms) {
    hits = results;
    cursor = 0;
    if (!results.length) {
      list.innerHTML = '';
      var empty = document.createElement('li');
      empty.className = 'palette-empty';
      empty.textContent = input.value.trim()
        ? 'Nothing matches “' + input.value.trim() + '”.'
        : 'Type to search the command reference.';
      list.appendChild(empty);
      return;
    }
    list.innerHTML = results.map(function (record, i) {
      return '<li role="option" aria-selected="' + (i === 0) + '">' +
             '<a href="' + href(record) + '">' +
             '<span class="r-crumb">' + escapeHtml(record.page) + '</span>' +
             '<span class="r-title">' + escapeHtml(record.title) + '</span>' +
             '<span class="r-text">' + excerpt(record, terms || []) + '</span>' +
             '</a></li>';
    }).join('');
  }

  function search(query) {
    var terms = query.toLowerCase().split(/\s+/).filter(Boolean);
    if (!terms.length || !records) { render([]); return; }
    var scored = [];
    for (var i = 0; i < records.length; i++) {
      var s = score(records[i], terms);
      if (s > 0) scored.push({ s: s, r: records[i] });
    }
    scored.sort(function (a, b) { return b.s - a.s; });
    // A long section is indexed as several overlapping windows so that a phrase
    // near its end still matches. The reader wants the section, not the windows:
    // keep the best-scoring one per heading and drop the rest.
    var seen = Object.create(null);
    var best = [];
    for (var j = 0; j < scored.length; j++) {
      var key = scored[j].r.slug + '#' + scored[j].r.anchor;
      if (seen[key]) continue;
      seen[key] = true;
      best.push(scored[j].r);
      if (best.length === 25) break;
    }
    render(best, terms);
  }

  function move(delta) {
    if (!hits.length) return;
    var items = list.querySelectorAll('li[role=option]');
    items[cursor].setAttribute('aria-selected', 'false');
    cursor = (cursor + delta + hits.length) % hits.length;
    items[cursor].setAttribute('aria-selected', 'true');
    items[cursor].scrollIntoView({ block: 'nearest' });
  }

  input.addEventListener('input', function () { search(input.value); });

  palette.addEventListener('keydown', function (event) {
    if (event.key === 'Escape') { event.preventDefault(); close(); }
    else if (event.key === 'ArrowDown') { event.preventDefault(); move(1); }
    else if (event.key === 'ArrowUp') { event.preventDefault(); move(-1); }
    else if (event.key === 'Enter' && hits.length) {
      event.preventDefault();
      location.href = href(hits[cursor]);
      close();
    }
  });

  palette.querySelectorAll('[data-palette-close]').forEach(function (el) {
    el.addEventListener('click', close);
  });
  document.querySelectorAll('[data-search-open]').forEach(function (el) {
    el.addEventListener('click', open);
    // Warm the index on intent, so the first keystroke has something to match.
    el.addEventListener('mouseenter', load, { once: true });
  });

  // ?q=… opens the palette pre-filled, so a search can be linked to.
  var initial = new URLSearchParams(location.search).get('q');
  if (initial) {
    open();
    input.value = initial;
    load().then(function () { search(initial); });
  }

  document.addEventListener('keydown', function (event) {
    var typing = /^(INPUT|TEXTAREA|SELECT)$/.test(event.target.tagName) || event.target.isContentEditable;
    if ((event.key === 'k' || event.key === 'K') && (event.metaKey || event.ctrlKey)) {
      event.preventDefault(); palette.hidden ? open() : close();
    } else if (event.key === '/' && !typing && palette.hidden) {
      event.preventDefault(); open();
    }
  });
})();
