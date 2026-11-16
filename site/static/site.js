(function () {
  var root = document.documentElement;

  // Theme: the stored choice wins, otherwise the system setting applies through CSS.
  var toggle = document.querySelector(".theme-toggle");
  if (toggle) toggle.addEventListener("click", function () {
    var dark = root.dataset.theme ? root.dataset.theme === "dark" : matchMedia("(prefers-color-scheme: dark)").matches;
    var next = dark ? "light" : "dark";
    root.dataset.theme = next;
    try { localStorage.setItem("theme", next); } catch (e) {}
  });

  var menu = document.querySelector(".menu-toggle");
  if (menu) menu.addEventListener("click", function () {
    var open = document.body.classList.toggle("menu-open");
    menu.setAttribute("aria-expanded", open);
  });

  // Screenshot lightbox.
  var box = document.querySelector(".lightbox");
  var big = box && box.querySelector("img");
  function close() { if (box) { box.hidden = true; big.removeAttribute("src"); } }
  document.addEventListener("click", function (e) {
    var link = e.target.closest && e.target.closest("[data-lightbox]");
    if (link && box) {
      e.preventDefault();
      big.src = link.href;
      big.alt = (link.querySelector("img") || {}).alt || "";
      box.hidden = false;
    } else if (box && !box.hidden && (e.target === box || e.target.tagName === "BUTTON")) {
      close();
    }
  });
  document.addEventListener("keydown", function (e) { if (e.key === "Escape") close(); });

  // Documentation search, over the index the build writes.
  var input = document.getElementById("doc-search");
  var results = document.getElementById("search-results");
  if (input) {
    var index = null;
    var base = document.querySelector('link[rel="stylesheet"]').getAttribute("href").replace("static/style.css", "");
    function load() {
      if (index) return Promise.resolve(index);
      return fetch(base + "search-index.json").then(function (r) { return r.json(); }).then(function (j) { return (index = j); });
    }
    function esc(s) { return s.replace(/[&<>"]/g, function (c) { return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]; }); }
    input.addEventListener("focus", load);
    input.addEventListener("input", function () {
      var q = input.value.trim().toLowerCase();
      if (!q) { results.hidden = true; return; }
      load().then(function (pages) {
        var words = q.split(/\s+/), hits = [];
        pages.forEach(function (p) {
          var title = p.t.toLowerCase(), heads = p.h.join(" ").toLowerCase(), text = p.x.toLowerCase(), score = 0;
          for (var i = 0; i < words.length; i++) {
            var w = words[i], s = 0;
            if (title.indexOf(w) >= 0) s += 8;
            if (heads.indexOf(w) >= 0) s += 4;
            if (text.indexOf(w) >= 0) s += 1;
            if (!s) { score = 0; break; }
            score += s;
          }
          if (score) {
            var at = text.indexOf(words[0]);
            var snip = at >= 0 ? p.x.slice(Math.max(0, at - 40), at + 90) : "";
            hits.push({ p: p, score: score, snip: snip });
          }
        });
        hits.sort(function (a, b) { return b.score - a.score; });
        results.innerHTML = hits.length
          ? hits.slice(0, 8).map(function (h) { return '<li><a href="' + base + h.p.u + '"><strong>' + esc(h.p.t) + "</strong><span>" + esc(h.snip) + "</span></a></li>"; }).join("")
          : "<li><span>No matches</span></li>";
        results.hidden = false;
      });
    });
  }

  // Highlight the table-of-contents entry for the section in view.
  var toc = document.querySelector(".toc");
  if (toc && "IntersectionObserver" in window) {
    var links = {};
    toc.querySelectorAll("a").forEach(function (a) { links[a.getAttribute("href").slice(1)] = a; });
    var seen = new IntersectionObserver(function (entries) {
      entries.forEach(function (en) {
        if (en.isIntersecting) {
          Object.keys(links).forEach(function (k) { links[k].classList.remove("active"); });
          var a = links[en.target.id]; if (a) a.classList.add("active");
        }
      });
    }, { rootMargin: "0px 0px -70% 0px" });
    document.querySelectorAll(".doc h2[id], .doc h3[id]").forEach(function (h) { if (links[h.id]) seen.observe(h); });
  }
})();
