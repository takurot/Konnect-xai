const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow } = window.__TAURI__.window;

// WebView2's native right-click menu includes its own "Reload", which
// reloads the whole embedded page (losing track of the open file — the
// startup-file argument is consumed on first read) rather than doing
// anything meaningful for a single-purpose viewer like this one.
document.addEventListener('contextmenu', (e) => e.preventDefault());

const MIN_SCALE = 0.05, MAX_SCALE = 40;

let scale = 1;
let panX = 0, panY = 0;
let isDragging = false;
let dragStartX, dragStartY;
let darkTheme = true;
let sheetSize = null;
let renderVersion = 0;

const viewer = document.getElementById('viewer');
const container = document.getElementById('svg-container');
const sheetImage = document.getElementById('schematic-image');
const zoomDisplay = document.getElementById('zoom-display');
const statusEl = document.getElementById('status');
const flashEl = document.getElementById('flash');
const emptyState = document.getElementById('empty-state');
const sheetSelector = document.getElementById('sheet-selector');
const filenameEl = document.getElementById('filename');
const loadingOverlay = document.getElementById('loading-overlay');
const loadingText = document.getElementById('loading-text');

// Unmistakable, centered loading state — a small "Rendering..." label in
// the corner reads as "the app might be hung" on a large, slow-to-render
// hierarchical design (real-world case: ~20 sheets took long enough that
// it looked frozen). This can't be missed the same way.
function showLoading(text) {
  loadingText.textContent = text || 'Rendering…';
  loadingOverlay.hidden = false;
  emptyState.style.display = 'none';
}
function hideLoading() {
  loadingOverlay.hidden = true;
}

function clampScale(s) {
  return Math.min(MAX_SCALE, Math.max(MIN_SCALE, s));
}

// Resize the image at layout size instead of scaling a low-resolution bitmap.
// WebKitGTK otherwise rasterizes the sheet at its pre-transform size.
function updateTransform() {
  container.style.transform = `translate(${panX}px, ${panY}px)`;
  if (sheetSize) {
    sheetImage.style.width = `${sheetSize.w * scale}px`;
    sheetImage.style.height = `${sheetSize.h * scale}px`;
  }
  zoomDisplay.textContent = Math.round(scale * 100) + '%';
}

function applySvgTheme() {
  sheetImage.style.filter = darkTheme ? 'invert(0.85) hue-rotate(180deg)' : 'none';
}

async function setSvg(svgContent, fit = false) {
  const version = ++renderVersion;
  // Only pass SVG to the browser image decoder, where scripting and external
  // resource loads are disabled. Keep the current sheet until decode succeeds.
  const nextImage = new Image();
  nextImage.src = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(svgContent);
  try {
    await nextImage.decode();
  } catch (error) {
    if (version !== renderVersion) return false;
    throw error;
  }
  if (version !== renderVersion) return false;
  sheetSize = { w: nextImage.naturalWidth, h: nextImage.naturalHeight };
  sheetImage.src = nextImage.src;
  sheetImage.hidden = false;
  emptyState.style.display = 'none';
  hideLoading();
  if (fit) fitToPage();
  else updateTransform();
  applySvgTheme();
  return true;
}

// `sticky` keeps the message up until the next flash replaces it — for
// in-progress states (re-rendering) rather than one-shot notifications.
// The single shared timer matters: without it, an earlier flash's
// hide-timeout fires late and wipes whatever message replaced it.
let flashTimer = null;
function flash(msg, isError, sticky) {
  flashEl.textContent = msg || 'Updated';
  flashEl.classList.toggle('error', !!isError);
  flashEl.classList.add('visible');
  if (flashTimer) {
    clearTimeout(flashTimer);
    flashTimer = null;
  }
  if (!sticky) {
    flashTimer = setTimeout(() => flashEl.classList.remove('visible'), isError ? 4000 : 2000);
  }
}

function fitToPage() {
  if (!sheetSize) return;
  const { w: svgW, h: svgH } = sheetSize;
  const viewW = viewer.clientWidth;
  const viewH = viewer.clientHeight;
  scale = clampScale(Math.min(viewW / svgW, viewH / svgH) * 0.95);
  panX = (viewW - svgW * scale) / 2;
  panY = (viewH - svgH * scale) / 2;
  updateTransform();
}

// Zoom keeping the sheet point under (x, y) fixed, in viewer coordinates.
function zoomAt(factor, x, y) {
  const newScale = clampScale(scale * factor);
  if (newScale === scale) return;
  panX = x - (x - panX) * (newScale / scale);
  panY = y - (y - panY) * (newScale / scale);
  scale = newScale;
  updateTransform();
}

// Buttons and keys zoom about the window center; anchored at the sheet's
// corner, which is off-screen once zoomed in, they slid the sheet away.
function zoomIn() { zoomAt(1.25, viewer.clientWidth / 2, viewer.clientHeight / 2); }
function zoomOut() { zoomAt(0.8, viewer.clientWidth / 2, viewer.clientHeight / 2); }

function toggleTheme() {
  darkTheme = !darkTheme;
  document.body.classList.toggle('light', !darkTheme);
  applySvgTheme();
}

async function openInKicad() {
  try {
    await invoke('open_in_kicad');
    statusEl.textContent = 'Opened in KiCAD';
  } catch (e) {
    statusEl.textContent = 'Error: ' + e;
    flash(String(e), true);
  }
}

// Mouse wheel zoom (centered on cursor)
viewer.addEventListener('wheel', (e) => {
  e.preventDefault();
  const rect = viewer.getBoundingClientRect();
  zoomAt(e.deltaY < 0 ? 1.1 : 0.9, e.clientX - rect.left, e.clientY - rect.top);
}, { passive: false });

// Mouse drag pan
viewer.addEventListener('mousedown', (e) => {
  isDragging = true;
  dragStartX = e.clientX - panX;
  dragStartY = e.clientY - panY;
  viewer.classList.add('dragging');
});

window.addEventListener('mousemove', (e) => {
  if (!isDragging) return;
  panX = e.clientX - dragStartX;
  panY = e.clientY - dragStartY;
  updateTransform();
});

window.addEventListener('mouseup', () => {
  isDragging = false;
  viewer.classList.remove('dragging');
});

// Keyboard shortcuts
window.addEventListener('keydown', (e) => {
  if (e.key === '+' || e.key === '=') zoomIn();
  else if (e.key === '-') zoomOut();
  else if (e.key === '0') fitToPage();
  else if (e.key === 'r' || e.key === 'R') manualRefresh();
});

async function manualRefresh() {
  try {
    statusEl.textContent = 'Refreshing...';
    showLoading('Refreshing...');
    const svg = await invoke('refresh');
    if (!await setSvg(svg)) return;
    flash('Refreshed');
    statusEl.textContent = 'Ready';
  } catch (e) {
    hideLoading();
    statusEl.textContent = 'Error: ' + e;
    flash(String(e), true);
  }
}

// ─── Sheet selector ────────────────────────────────────────────────────

// Depth-indented flat list, in the same depth-first order the backend's
// walk (and sch_hierarchy's get_sheet_hierarchy/renumber_sheet_pages)
// already use — not a collapsible tree, see the design note for why.
function populateSheetSelector(sheets, activeFile) {
  sheetSelector.replaceChildren();
  if (!sheets || sheets.length <= 1) {
    sheetSelector.hidden = true;
    return;
  }
  for (const s of sheets) {
    const opt = document.createElement('option');
    opt.value = s.file;
    // Plain spaces collapse to a single space inside <option> text (normal HTML whitespace rules apply even here), so indentation below uses non-breaking spaces, not regular ones, to actually show up.
    const indent = '    '.repeat(s.depth);
    const marker = s.depth > 0 ? '└─ ' : ''; // box-drawing tree marker
    opt.textContent = indent + marker + s.name;
    sheetSelector.appendChild(opt);
  }
  sheetSelector.value = activeFile;
  sheetSelector.hidden = false;
}

async function onSheetSelected() {
  const file = sheetSelector.value;
  try {
    statusEl.textContent = 'Loading sheet...';
    showLoading('Loading sheet...');
    const svg = await invoke('select_sheet', { file });
    if (!await setSvg(svg, true)) return;
    statusEl.textContent = 'Watching for changes...';
  } catch (e) {
    hideLoading();
    statusEl.textContent = 'Error: ' + e;
    flash(String(e), true);
  }
}

document.getElementById('zoom-in').addEventListener('click', zoomIn);
document.getElementById('zoom-out').addEventListener('click', zoomOut);
document.getElementById('fit-to-page').addEventListener('click', fitToPage);
document.getElementById('refresh').addEventListener('click', manualRefresh);
document.getElementById('theme').addEventListener('click', toggleTheme);
document.getElementById('open-in-kicad').addEventListener('click', openInKicad);
sheetSelector.addEventListener('change', onSheetSelected);

// Live updates and errors from the Rust backend.
//
// listen() returns a promise that REJECTS if the event API isn't permitted
// by the app's Tauri capability file — and a top-level unhandled rejection
// is completely silent. That exact failure shipped once: with no
// capabilities/ dir, every listener below was dead, the backend emitted
// into a void, and live updates simply "didn't work" with nothing in any
// log. Surface it unmissably instead.
function listenOrWarn(name, handler) {
  listen(name, handler).catch((e) => {
    statusEl.textContent = 'Event wiring failed';
    flash('Event wiring failed for "' + name + '": ' + e, true);
  });
}

listenOrWarn('schematic-updated', async (event) => {
  try {
    if (!await setSvg(event.payload)) return;
    flash('File changed — updated');
    statusEl.textContent = 'Live';
  } catch (e) {
    hideLoading();
    statusEl.textContent = 'Error: ' + e;
    flash(String(e), true);
  }
});

// Fired whenever the watcher re-walks the tree — keeps the selector in
// sync without requiring the viewer to be reopened. `active_file` reflects
// any backend fallback-to-root (e.g. the selected sheet was deleted).
// Emitted at the end of every render cycle (whether or not the active
// sheet changed) — so it also retires the sticky "re-rendering" flash.
// The retirement is deferred a beat: when the active sheet WAS
// re-rendered, schematic-updated lands right behind this event and takes
// over the flash — showing "Re-render complete" for a split second first
// just reads as flicker. It only appears when it's genuinely the final
// word (the edit touched sheets other than the one on screen).
listenOrWarn('sheets-updated', (event) => {
  populateSheetSelector(event.payload.sheets, event.payload.active_file);
  statusEl.textContent = 'Live';
  setTimeout(() => {
    if (flashEl.classList.contains('visible') && flashEl.textContent === RENDERING_MSG) {
      flash('Re-render complete');
    }
  }, 150);
});

// Fired the instant the backend's watcher notices a relevant file change,
// before any rendering starts (which can take a while on a big design) -
// makes "watcher never fired" visibly different from "render failed".
const RENDERING_MSG = 'Change detected - re-rendering...';
listenOrWarn('change-detected', () => {
  statusEl.textContent = RENDERING_MSG;
  // Sticky: stays up for the whole render (can be several seconds on a
  // big design) so the user watches a state, not a vanishing blip. The
  // sheets-updated handler below retires it when the cycle finishes.
  flash(RENDERING_MSG, false, true);
});

listenOrWarn('viewer-error', (event) => {
  statusEl.textContent = 'Error';
  flash(String(event.payload), true);
});

// Drag & drop a .kicad_sch onto the window
getCurrentWindow().onDragDropEvent((event) => {
  const t = event.payload.type;
  if (t === 'enter' || t === 'over') {
    viewer.classList.add('drop-target');
  } else if (t === 'leave') {
    viewer.classList.remove('drop-target');
  } else if (t === 'drop') {
    viewer.classList.remove('drop-target');
    const path = (event.payload.paths || []).find(p => p.toLowerCase().endsWith('.kicad_sch'));
    if (path) {
      window.loadSchematic(path);
    } else {
      flash('Not a .kicad_sch file', true);
    }
  }
});

// Called on startup (with the CLI-arg file, if any) and on drag-drop.
// Opens the ROOT of a design — the backend walks the full sheet tree from
// there and eagerly renders every sheet it finds.
window.loadSchematic = async function(path) {
  try {
    filenameEl.textContent = path.split(/[\\/]/).pop();
    statusEl.textContent = 'Rendering...';
    showLoading('Rendering... (a large hierarchical design can take a while — every sheet is rendered up front)');
    const result = await invoke('open_schematic', { path });
    populateSheetSelector(result.sheets, result.active_file);
    if (!await setSvg(result.svg, true)) return;
    if (result.render_errors && result.render_errors.length) {
      flash(`${result.render_errors.length} sheet(s) failed to render`, true);
    }
    statusEl.textContent = 'Watching for changes...';
  } catch (e) {
    hideLoading();
    statusEl.textContent = 'Error: ' + e;
    flash(String(e), true);
    console.error(e);
  }
};

// Startup: ask the backend for the CLI-arg file. This replaces the old
// sleep-and-eval handoff — the frontend pulls when it's ready.
(async function init() {
  try {
    const startupFile = await invoke('get_startup_file');
    if (startupFile) {
      await window.loadSchematic(startupFile);
    } else {
      statusEl.textContent = 'Ready — drop a file or use the open_schematic_viewer MCP tool';
    }
  } catch (e) {
    console.error(e);
  }
})();
