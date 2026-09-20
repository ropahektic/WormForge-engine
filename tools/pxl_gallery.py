#!/usr/bin/env python3
"""Build a searchable HTML inspector for the rendered sprite GIFs.

Writes a single self-contained page next to the GIF folder, so that the page
resolves its images at <out-dir>/pxl_gifs/<lib>__<sprite>.gif. The original
sprite sheets are referenced at pxl_extracted/<lib>/files/<file> when that
folder is a sibling too; the page hides the sheet view when it is not.

Usage: pxl_gallery.py <extracted-dir> <gif-dir> [out.html]
"""
import json
import os
import sys

from PIL import Image, ImageFile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pxl_gifs import IMG_EXT, frame_layout, script_frame_counts   # noqa: E402

ImageFile.LOAD_TRUNCATED_IMAGES = True


def collect(extracted, gif_dir):
    """One record per rendered sprite: identity, frame geometry, provenance."""
    out = []
    for lib in sorted(os.listdir(extracted)):
        files_dir = os.path.join(extracted, lib, 'files')
        if not os.path.isdir(files_dir):
            continue
        decls = script_frame_counts(os.path.join(extracted, lib))
        for fn in sorted(os.listdir(files_dir)):
            if not fn.lower().endswith(IMG_EXT):
                continue
            gif = ('%s__%s.gif' % (lib, os.path.splitext(fn)[0])).replace('/', '_')
            path = os.path.join(gif_dir, gif)
            if not os.path.exists(path):
                continue
            try:
                w, h = Image.open(path).size          # gif size == frame size
                src = Image.open(os.path.join(files_dir, fn))
                sw, sh = src.size
                # The GIF's own frame count understates sheets with repeated
                # frames, which the writer merges (keeping total duration), so
                # take the count from the sheet the same way the renderer did.
                frames = (src.n_frames if getattr(src, 'n_frames', 1) > 1
                          else frame_layout((sw, sh), decls.get(fn.lower()))[0])
            except Exception:
                continue
            out.append({
                'g': gif,                       # gif filename
                'l': lib,                       # library
                'f': fn,                        # source sheet filename
                'w': w, 'h': h,                 # frame size
                'n': frames,                    # frames in the sheet
                'sw': sw, 'sh': sh,             # sheet size
                'b': os.path.getsize(path),     # gif bytes
                'd': 1 if fn.lower() in decls else 0,   # frame count declared?
            })
    return out


TEMPLATE = r"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>PXL Sprite Inspector</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400;500;600&family=IBM+Plex+Sans+Condensed:wght@600;700&family=IBM+Plex+Sans:wght@400;500;600&display=swap">
<style>
:root{
  --ground:#e6eaeb; --panel:#f7f9f9; --panel-2:#ffffff; --line:#c9d2d4;
  --ink:#10181a; --ink-dim:#5a686d; --ink-faint:#8a969a;
  --accent:#a8600c; --accent-soft:#f0dcc0; --signal:#0d6f60;
  --cell:#dfe5e6; --on-accent:#ffffff;
  --shadow:0 1px 2px rgba(16,24,26,.09),0 6px 18px rgba(16,24,26,.06);
}
@media (prefers-color-scheme: dark){
  :root:not([data-theme="light"]){
    --ground:#101416; --panel:#171d20; --panel-2:#1d2528; --line:#2b3539;
    --ink:#e2e9ea; --ink-dim:#93a2a7; --ink-faint:#68787d;
    --accent:#f0a93b; --accent-soft:#3a2c14; --signal:#45cdb3; --on-accent:#1a1206;
    --cell:#0b0e10; --shadow:0 1px 2px rgba(0,0,0,.4),0 8px 22px rgba(0,0,0,.28);
  }
}
:root[data-theme="dark"]{
  --ground:#101416; --panel:#171d20; --panel-2:#1d2528; --line:#2b3539;
  --ink:#e2e9ea; --ink-dim:#93a2a7; --ink-faint:#68787d;
  --accent:#f0a93b; --accent-soft:#3a2c14; --signal:#45cdb3; --on-accent:#1a1206;
  --cell:#0b0e10; --shadow:0 1px 2px rgba(0,0,0,.4),0 8px 22px rgba(0,0,0,.28);
}
*{box-sizing:border-box}
body{
  margin:0; background:var(--ground); color:var(--ink);
  font-family:"IBM Plex Sans",-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;
  font-size:14px; line-height:1.5; -webkit-font-smoothing:antialiased;
}
h1,h2,.cond{font-family:"IBM Plex Sans Condensed","IBM Plex Sans",sans-serif}
.mono,code{font-family:"IBM Plex Mono",ui-monospace,Menlo,monospace;font-variant-numeric:tabular-nums}

/* ---- masthead + controls ---------------------------------------------- */
header{
  position:sticky; top:0; z-index:30; background:var(--panel);
  border-bottom:1px solid var(--line);
}
.bar{display:flex; align-items:baseline; gap:14px; flex-wrap:wrap;
     padding:12px 20px 0}
h1{margin:0; font-size:19px; font-weight:700; letter-spacing:-.01em}
h1 .tag{color:var(--accent)}
.counts{font-size:12px; color:var(--ink-dim); letter-spacing:.02em}
.counts b{color:var(--ink); font-weight:600}
.spacer{flex:1}
.controls{display:flex; gap:10px; flex-wrap:wrap; align-items:center;
          padding:10px 20px 12px}
.field{position:relative; display:flex; align-items:center}
.field svg{position:absolute; left:9px; width:14px; height:14px; color:var(--ink-faint)}
input[type=search],select{
  font:inherit; color:var(--ink); background:var(--panel-2);
  border:1px solid var(--line); border-radius:3px; padding:7px 10px;
}
input[type=search]{padding-left:28px; width:300px; max-width:52vw;
  font-family:"IBM Plex Mono",monospace; font-size:13px}
input[type=search]::-webkit-search-cancel-button{filter:grayscale(1)}
select{max-width:240px}
input:focus-visible,select:focus-visible,button:focus-visible,.cell:focus-visible{
  outline:2px solid var(--accent); outline-offset:2px}
.seg{display:flex; border:1px solid var(--line); border-radius:3px; overflow:hidden}
.seg button{
  font:inherit; font-size:12px; padding:7px 11px; border:0; cursor:pointer;
  background:var(--panel-2); color:var(--ink-dim); border-right:1px solid var(--line);
}
.seg button:last-child{border-right:0}
.seg button[aria-pressed=true]{background:var(--accent); color:var(--on-accent)}
.lbl{font-size:11px; text-transform:uppercase; letter-spacing:.09em;
     color:var(--ink-faint); font-weight:600}
input[type=range]{accent-color:var(--accent); width:96px}

/* ---- grid -------------------------------------------------------------- */
main{padding:18px 20px 60px}
#grid{display:grid; gap:10px;
      grid-template-columns:repeat(auto-fill,minmax(var(--tile,120px),1fr))}
.cell{
  display:flex; flex-direction:column; background:var(--panel); border:1px solid var(--line);
  border-radius:3px; padding:0; cursor:pointer; text-align:left; font:inherit;
  color:inherit; overflow:hidden; transition:border-color .12s, transform .12s;
}
.cell:hover{border-color:var(--accent); transform:translateY(-1px)}
.cell[aria-selected=true]{border-color:var(--accent); box-shadow:0 0 0 1px var(--accent)}
.stage{
  position:relative; height:var(--tile,120px); display:grid; place-items:center;
  background:var(--cell); overflow:hidden;
}
.stage.checker{
  background-image:linear-gradient(45deg,#8d9699 25%,transparent 25%),
    linear-gradient(-45deg,#8d9699 25%,transparent 25%),
    linear-gradient(45deg,transparent 75%,#8d9699 75%),
    linear-gradient(-45deg,transparent 75%,#8d9699 75%);
  background-size:16px 16px;
  background-position:0 0,0 8px,8px -8px,-8px 0; background-color:#c6cfd1;
}
.stage.dark{background:#0b0e10}
.stage.light{background:#f2f4f4}
.stage.magenta{background:#ff00ff}
.stage img{image-rendering:pixelated; display:block}
.badge{
  position:absolute; top:5px; right:5px; font-size:10px; font-weight:600;
  padding:1px 5px; border-radius:2px; background:var(--signal); color:var(--ground);
  font-family:"IBM Plex Mono",monospace; letter-spacing:.02em;
}
.cap{padding:6px 8px; border-top:1px solid var(--line); min-width:0}
.cap .nm{font-size:11.5px; font-family:"IBM Plex Mono",monospace;
  white-space:nowrap; overflow:hidden; text-overflow:ellipsis}
.cap .lb{font-size:10px; color:var(--ink-faint); white-space:nowrap;
  overflow:hidden; text-overflow:ellipsis; letter-spacing:.02em}
#sentinel{height:1px}
.empty{padding:70px 0; text-align:center; color:var(--ink-dim)}
.empty p{margin:6px 0}
.more{display:block; margin:22px auto 0; font:inherit; font-size:13px; padding:9px 18px;
  background:var(--panel); border:1px solid var(--line); border-radius:3px;
  color:var(--ink); cursor:pointer}
.more:hover{border-color:var(--accent)}

/* ---- detail drawer ----------------------------------------------------- */
#drawer{
  position:fixed; top:0; right:0; width:min(380px,92vw); height:100%; z-index:40;
  background:var(--panel); border-left:1px solid var(--line); box-shadow:var(--shadow);
  transform:translateX(102%); transition:transform .2s ease; display:flex;
  flex-direction:column; overflow-y:auto;
}
#drawer.open{transform:none}
@media (prefers-reduced-motion:reduce){
  #drawer{transition:none} .cell{transition:none}
}
.dhead{display:flex; align-items:flex-start; gap:10px; padding:14px 16px 10px;
  border-bottom:1px solid var(--line); position:sticky; top:0; background:var(--panel)}
.dhead h2{margin:0; font-size:15px; word-break:break-all; line-height:1.3}
.dhead .lb{font-size:11px; color:var(--ink-faint); font-family:"IBM Plex Mono",monospace}
.x{margin-left:auto; background:none; border:0; color:var(--ink-dim); cursor:pointer;
   font-size:20px; line-height:1; padding:2px 4px; border-radius:3px}
.x:hover{color:var(--ink)}
.dstage{display:grid; place-items:center; min-height:190px; padding:18px;
  border-bottom:1px solid var(--line)}
.dstage img{image-rendering:pixelated; max-width:100%}
dl{margin:0; padding:12px 16px; display:grid; grid-template-columns:auto 1fr;
   gap:7px 14px; font-size:12.5px; border-bottom:1px solid var(--line)}
dt{color:var(--ink-faint); font-size:11px; text-transform:uppercase;
   letter-spacing:.07em; font-weight:600; padding-top:1px}
dd{margin:0; font-family:"IBM Plex Mono",monospace; word-break:break-all}
.pill{display:inline-block; font-size:10px; padding:1px 6px; border-radius:2px;
  background:var(--accent-soft); color:var(--accent); font-weight:600;
  letter-spacing:.03em; text-transform:uppercase}
.sheet{padding:12px 16px 26px}
.sheet .lbl{display:block; margin-bottom:8px}
.sheetbox{background:var(--cell); border:1px solid var(--line); border-radius:3px;
  max-height:340px; overflow:auto; padding:8px; display:grid; place-items:start center}
.sheet img{image-rendering:pixelated}
.nav{display:flex; gap:8px; padding:0 16px 14px}
.nav button{flex:1; font:inherit; font-size:12px; padding:7px; cursor:pointer;
  background:var(--panel-2); border:1px solid var(--line); border-radius:3px; color:var(--ink)}
.nav button:hover{border-color:var(--accent)}
kbd{font-family:"IBM Plex Mono",monospace; font-size:10px; border:1px solid var(--line);
  border-bottom-width:2px; border-radius:3px; padding:0 4px; color:var(--ink-dim)}
</style>
</head>
<body>
<header>
  <div class="bar">
    <h1>PXL Sprite <span class="tag">Inspector</span></h1>
    <div class="counts" id="counts"></div>
    <div class="spacer"></div>
    <div class="counts">press <kbd>/</kbd> to search &middot; <kbd>esc</kbd> to close</div>
  </div>
  <div class="controls">
    <label class="field">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><circle cx="11" cy="11" r="7"/><path d="M20 20l-3.5-3.5"/></svg>
      <input type="search" id="q" placeholder="search sprites &amp; libraries" aria-label="Search sprites and libraries" autocomplete="off">
    </label>
    <select id="lib" aria-label="Filter by library"></select>
    <div class="seg" role="group" aria-label="Animation filter">
      <button data-kind="all" aria-pressed="true">All</button>
      <button data-kind="anim" aria-pressed="false">Animated</button>
      <button data-kind="still" aria-pressed="false">Static</button>
    </div>
    <select id="sort" aria-label="Sort order">
      <option value="name">Sort: name</option>
      <option value="frames">Sort: most frames</option>
      <option value="size">Sort: largest file</option>
      <option value="dim">Sort: largest frame</option>
      <option value="lib">Sort: library</option>
    </select>
    <span class="lbl">Backdrop</span>
    <div class="seg" role="group" aria-label="Backdrop">
      <button data-bg="checker" aria-pressed="true">Checker</button>
      <button data-bg="dark" aria-pressed="false">Dark</button>
      <button data-bg="light" aria-pressed="false">Light</button>
      <button data-bg="magenta" aria-pressed="false">Magenta</button>
    </div>
    <span class="lbl">Zoom</span>
    <input type="range" id="zoom" min="72" max="260" step="4" value="120" aria-label="Tile size">
  </div>
</header>

<main>
  <div id="grid"></div>
  <div id="sentinel"></div>
  <div class="empty" id="empty" hidden>
    <p class="cond" style="font-size:17px">No sprites match that search.</p>
    <p>Try a shorter term, or clear the library filter.</p>
  </div>
</main>

<aside id="drawer" aria-label="Sprite detail" aria-hidden="true">
  <div class="dhead">
    <div>
      <h2 id="d-name"></h2>
      <div class="lb" id="d-lib"></div>
    </div>
    <button class="x" id="d-close" aria-label="Close detail">&times;</button>
  </div>
  <div class="dstage" id="d-stage"><img id="d-img" alt=""></div>
  <dl>
    <dt>Frames</dt><dd id="d-frames"></dd>
    <dt>Frame size</dt><dd id="d-size"></dd>
    <dt>Sheet</dt><dd id="d-sheet"></dd>
    <dt>GIF</dt><dd id="d-bytes"></dd>
    <dt>Source</dt><dd id="d-src"></dd>
  </dl>
  <div class="nav">
    <button id="d-prev">&larr; Previous</button>
    <button id="d-next">Next &rarr;</button>
  </div>
  <div class="sheet" id="d-sheetwrap" hidden>
    <span class="lbl">Original sheet</span>
    <div class="sheetbox"><img id="d-sheetimg" alt="Original sprite sheet"></div>
  </div>
</aside>

<script>
const DATA = __MANIFEST__;
const GIFS = "__GIFDIR__", SHEETS = "__SHEETDIR__";
const CHUNK = 120;

const grid = document.getElementById('grid');
const qEl = document.getElementById('q');
const libEl = document.getElementById('lib');
const sortEl = document.getElementById('sort');
const zoomEl = document.getElementById('zoom');
const emptyEl = document.getElementById('empty');
const countsEl = document.getElementById('counts');

let kind = 'all', bg = 'checker', view = [], shown = 0, selected = -1;

/* Library picker, most populous first so the big sets are easy to reach. */
const byLib = new Map();
for (const s of DATA) byLib.set(s.l, (byLib.get(s.l) || 0) + 1);
libEl.innerHTML = '<option value="">All libraries (' + byLib.size + ')</option>' +
  [...byLib.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .map(([l, n]) => '<option value="' + l + '">' + l + ' · ' + n + '</option>').join('');

const animCount = DATA.filter(s => s.n > 1).length;
countsEl.innerHTML = '<b>' + DATA.length + '</b> sprites &middot; <b>' + byLib.size +
  '</b> libraries &middot; <b>' + animCount + '</b> animated';

/* Attach src only as tiles approach the viewport: 3182 GIFs must not all load. */
const io = new IntersectionObserver(es => {
  for (const e of es) {
    if (!e.isIntersecting) continue;
    const img = e.target;
    if (img.dataset.src) { img.src = img.dataset.src; delete img.dataset.src; }
    io.unobserve(img);
  }
}, { rootMargin: '700px 0px' });

const bytes = b => b < 1024 ? b + ' B'
  : b < 1048576 ? (b / 1024).toFixed(1) + ' KB' : (b / 1048576).toFixed(2) + ' MB';

/* Pixel art deserves integer scaling; only shrink when a frame overflows. */
function fit(img, w, h, tile) {
  const box = tile - 8;
  if (w > box || h > box) {
    img.style.width = ''; img.style.height = '';
    img.style.maxWidth = box + 'px'; img.style.maxHeight = box + 'px';
  } else {
    const s = Math.max(1, Math.floor(box / Math.max(w, h)));
    img.style.maxWidth = ''; img.style.maxHeight = '';
    img.style.width = (w * s) + 'px'; img.style.height = (h * s) + 'px';
  }
}

function apply() {
  writeURL();
  const terms = qEl.value.toLowerCase().split(/\s+/).filter(Boolean);
  const lib = libEl.value;
  view = DATA.filter(s => {
    if (lib && s.l !== lib) return false;
    if (kind === 'anim' && s.n < 2) return false;
    if (kind === 'still' && s.n > 1) return false;
    if (!terms.length) return true;
    const hay = (s.l + ' ' + s.f).toLowerCase();
    return terms.every(t => hay.includes(t));
  });
  const by = sortEl.value;
  view.sort(by === 'frames' ? (a, b) => b.n - a.n || a.f.localeCompare(b.f)
    : by === 'size' ? (a, b) => b.b - a.b
    : by === 'dim' ? (a, b) => (b.w * b.h) - (a.w * a.h)
    : by === 'lib' ? (a, b) => a.l.localeCompare(b.l) || a.f.localeCompare(b.f)
    : (a, b) => a.f.localeCompare(b.f) || a.l.localeCompare(b.l));
  grid.textContent = ''; shown = 0; selected = -1;
  emptyEl.hidden = view.length > 0;
  document.querySelectorAll('.more').forEach(n => n.remove());
  render();
}

function render() {
  const tile = +zoomEl.value;
  const frag = document.createDocumentFragment();
  const end = Math.min(shown + CHUNK, view.length);
  for (let i = shown; i < end; i++) {
    const s = view[i];
    const cell = document.createElement('button');
    cell.className = 'cell'; cell.dataset.i = i; cell.type = 'button';
    cell.title = s.l + ' / ' + s.f;
    const stage = document.createElement('div');
    stage.className = 'stage ' + bg;
    const img = document.createElement('img');
    img.dataset.src = GIFS + '/' + encodeURIComponent(s.g);
    img.alt = s.f; img.loading = 'lazy';
    fit(img, s.w, s.h, tile);
    stage.appendChild(img);
    if (s.n > 1) {
      const b = document.createElement('span');
      b.className = 'badge'; b.textContent = s.n + 'f';
      stage.appendChild(b);
    }
    const cap = document.createElement('div');
    cap.className = 'cap';
    const nm = document.createElement('div'); nm.className = 'nm'; nm.textContent = s.f;
    const lb = document.createElement('div'); lb.className = 'lb'; lb.textContent = s.l;
    cap.append(nm, lb);
    cell.append(stage, cap);
    frag.appendChild(cell);
    io.observe(img);
  }
  grid.appendChild(frag);
  shown = end;
  document.querySelectorAll('.more').forEach(n => n.remove());
  if (shown < view.length) {
    const b = document.createElement('button');
    b.className = 'more';
    b.textContent = 'Show more — ' + (view.length - shown) + ' remaining';
    b.onclick = render;
    grid.after(b);
  }
}

/* Infinite scroll, with the button above as the explicit fallback. */
new IntersectionObserver(es => {
  if (es[0].isIntersecting && shown < view.length) render();
}, { rootMargin: '400px' }).observe(document.getElementById('sentinel'));

/* ---- detail drawer ---- */
const drawer = document.getElementById('drawer');
const dImg = document.getElementById('d-img');
const dStage = document.getElementById('d-stage');
const sheetWrap = document.getElementById('d-sheetwrap');
const sheetImg = document.getElementById('d-sheetimg');

function openDetail(i) {
  if (i < 0 || i >= view.length) return;
  if (i >= shown) { while (shown <= i && shown < view.length) render(); }
  const s = view[i];
  document.querySelectorAll('.cell[aria-selected=true]').forEach(c => c.removeAttribute('aria-selected'));
  const cell = grid.querySelector('.cell[data-i="' + i + '"]');
  if (cell) { cell.setAttribute('aria-selected', 'true'); cell.scrollIntoView({ block: 'nearest' }); }
  selected = i;
  document.getElementById('d-name').textContent = s.f;
  document.getElementById('d-lib').textContent = s.l;
  dStage.className = 'dstage ' + bg;
  dImg.src = GIFS + '/' + encodeURIComponent(s.g);
  const s2 = Math.max(1, Math.floor(300 / Math.max(s.w, s.h)));
  dImg.style.width = (s.w * s2) + 'px'; dImg.style.height = (s.h * s2) + 'px';
  document.getElementById('d-frames').innerHTML = s.n > 1
    ? s.n + ' <span class="pill">' + (s.d ? 'declared' : 'inferred') + '</span>'
    : '1 <span class="pill">static</span>';
  document.getElementById('d-size').textContent = s.w + ' × ' + s.h + ' px';
  document.getElementById('d-sheet').textContent = s.sw + ' × ' + s.sh + ' px';
  document.getElementById('d-bytes').textContent = bytes(s.b);
  document.getElementById('d-src').textContent = s.l + '/files/' + s.f;
  sheetWrap.hidden = true;
  sheetImg.onload = () => { sheetWrap.hidden = false; };
  sheetImg.onerror = () => { sheetWrap.hidden = true; };
  sheetImg.src = SHEETS + '/' + encodeURIComponent(s.l) + '/files/' + encodeURIComponent(s.f);
  drawer.classList.add('open'); drawer.setAttribute('aria-hidden', 'false');
}
function closeDetail() {
  drawer.classList.remove('open'); drawer.setAttribute('aria-hidden', 'true');
  document.querySelectorAll('.cell[aria-selected=true]').forEach(c => c.removeAttribute('aria-selected'));
}

grid.addEventListener('click', e => {
  const cell = e.target.closest('.cell');
  if (cell) openDetail(+cell.dataset.i);
});
document.getElementById('d-close').onclick = closeDetail;
document.getElementById('d-prev').onclick = () => openDetail(selected - 1);
document.getElementById('d-next').onclick = () => openDetail(selected + 1);

/* ---- controls ---- */
let t;
qEl.addEventListener('input', () => { clearTimeout(t); t = setTimeout(apply, 130); });
libEl.onchange = apply;
sortEl.onchange = apply;
document.querySelectorAll('[data-kind]').forEach(b => b.onclick = () => {
  kind = b.dataset.kind;
  document.querySelectorAll('[data-kind]').forEach(o =>
    o.setAttribute('aria-pressed', String(o === b)));
  apply();
});
document.querySelectorAll('[data-bg]').forEach(b => b.onclick = () => {
  bg = b.dataset.bg;
  document.querySelectorAll('[data-bg]').forEach(o =>
    o.setAttribute('aria-pressed', String(o === b)));
  document.querySelectorAll('.stage').forEach(s => s.className = 'stage ' + bg);
  dStage.className = 'dstage ' + bg;
  writeURL();
});
zoomEl.addEventListener('input', () => {
  const tile = +zoomEl.value;
  document.documentElement.style.setProperty('--tile', tile + 'px');
  grid.querySelectorAll('.cell').forEach(c => {
    const s = view[+c.dataset.i];
    if (s) fit(c.querySelector('img'), s.w, s.h, tile);
  });
  writeURL();
});
document.addEventListener('keydown', e => {
  if (e.key === 'Escape') { closeDetail(); return; }
  if (e.key === '/' && document.activeElement !== qEl) { e.preventDefault(); qEl.focus(); qEl.select(); return; }
  if (drawer.classList.contains('open') && document.activeElement !== qEl) {
    if (e.key === 'ArrowRight') { e.preventDefault(); openDetail(selected + 1); }
    if (e.key === 'ArrowLeft') { e.preventDefault(); openDetail(selected - 1); }
  }
});

/* State lives in the URL so a particular view can be bookmarked or shared. */
function readURL() {
  const p = new URLSearchParams(location.search);
  if (p.has('q')) qEl.value = p.get('q');
  if (p.has('lib') && [...libEl.options].some(o => o.value === p.get('lib')))
    libEl.value = p.get('lib');
  if (p.has('sort')) sortEl.value = p.get('sort');
  if (p.has('zoom')) zoomEl.value = p.get('zoom');
  if (p.has('kind')) {
    const b = document.querySelector('[data-kind="' + p.get('kind') + '"]');
    if (b) { kind = p.get('kind');
      document.querySelectorAll('[data-kind]').forEach(o =>
        o.setAttribute('aria-pressed', String(o === b))); }
  }
  if (p.has('bg')) {
    const b = document.querySelector('[data-bg="' + p.get('bg') + '"]');
    if (b) { bg = p.get('bg');
      document.querySelectorAll('[data-bg]').forEach(o =>
        o.setAttribute('aria-pressed', String(o === b)));
      dStage.className = 'dstage ' + bg; }
  }
}
function writeURL() {
  const p = new URLSearchParams();
  if (qEl.value) p.set('q', qEl.value);
  if (libEl.value) p.set('lib', libEl.value);
  if (kind !== 'all') p.set('kind', kind);
  if (sortEl.value !== 'name') p.set('sort', sortEl.value);
  if (bg !== 'checker') p.set('bg', bg);
  if (zoomEl.value !== '120') p.set('zoom', zoomEl.value);
  const qs = p.toString();
  // Some browsers refuse history writes on file:// URLs; the view still works.
  try { history.replaceState(null, '', qs ? '?' + qs : location.pathname); }
  catch (e) { /* no deep links here, nothing else is affected */ }
}

readURL();
document.documentElement.style.setProperty('--tile', zoomEl.value + 'px');
apply();
</script>
</body>
</html>
"""


def main(argv):
    if len(argv) < 3:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    extracted, gif_dir = argv[1], argv[2]
    out = argv[3] if len(argv) > 3 else os.path.join(
        os.path.dirname(os.path.abspath(gif_dir)), 'sprites.html')
    data = collect(extracted, gif_dir)
    if not data:
        print('no rendered sprites found', file=sys.stderr)
        return 1
    html = (TEMPLATE
            .replace('__MANIFEST__', json.dumps(data, separators=(',', ':')))
            .replace('__GIFDIR__', os.path.basename(os.path.normpath(gif_dir)))
            .replace('__SHEETDIR__', os.path.basename(os.path.normpath(extracted))))
    with open(out, 'w', encoding='utf-8') as f:
        f.write(html)
    print('wrote %s (%d sprites, %.0f KB)' % (out, len(data), len(html) / 1024))
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
