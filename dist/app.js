(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const { open: openDialog } = window.__TAURI__.dialog;
  const { startDrag } = window.__TAURI__.drag;

  const el = (id) => document.getElementById(id);

  function loadPref(key, fallback) {
    try {
      const v = localStorage.getItem(key);
      return v === null ? fallback : JSON.parse(v);
    } catch {
      return fallback;
    }
  }
  function savePref(key, value) {
    try {
      localStorage.setItem(key, JSON.stringify(value));
    } catch {
      /* ignore (private mode, quota, etc.) */
    }
  }

  const state = {
    view: "browse",
    roots: [],
    scanning: new Map(), // root_path -> {processed,total,current_file}
    categories: [],
    selectedCategories: new Set(),
    soundTypes: [],
    selectedSoundTypes: new Set(),
    folderTree: [],
    selectedFolder: null, // {root, folder}
    folderSearchText: "",
    collapsedFolders: new Set(),
    searchText: "",
    minSecs: null,
    maxSecs: null,
    favoritesOnly: false,
    results: [],
    offset: 0,
    limit: 200,
    selectedFile: null,
    isPlaying: false,
    autoplay: loadPref("fx.autoplay", true),
    loopPlayback: loadPref("fx.loop", true),
    playStartedAt: null,
    waveform: null,
  };

  const tabButtons = document.querySelectorAll(".tab-btn");

  function switchView(view) {
    state.view = view;
    el("browse-view").hidden = view !== "browse";
    el("settings-view").hidden = view !== "settings";
    tabButtons.forEach((b) => b.classList.toggle("active", b.dataset.view === view));
    if (view === "settings") renderRoots();
    if (view === "browse") {
      refreshFacets();
      runSearch(true);
    }
  }
  tabButtons.forEach((b) => b.addEventListener("click", () => switchView(b.dataset.view)));

  // ---------------- Settings / roots ----------------
  async function loadRoots() {
    state.roots = await invoke("list_roots");
    renderRoots();
    updateFooter();
  }

  function fmtDate(ts) {
    if (!ts) return "never";
    return new Date(ts * 1000).toLocaleString();
  }

  function button(cls, text, onClick) {
    const b = document.createElement("button");
    b.className = cls;
    b.textContent = text;
    b.addEventListener("click", onClick);
    return b;
  }

  function renderRoots() {
    const list = el("roots-list");
    list.innerHTML = "";
    if (state.roots.length === 0) {
      const p = document.createElement("p");
      p.className = "hint";
      p.textContent = "No folders added yet.";
      list.appendChild(p);
      return;
    }
    for (const r of state.roots) {
      const card = document.createElement("div");
      card.className = "root-card";

      const info = document.createElement("div");
      info.className = "root-info";
      const pathEl = document.createElement("div");
      pathEl.className = "root-path";
      pathEl.textContent = r.root_path;
      const metaEl = document.createElement("div");
      metaEl.className = "root-meta";
      metaEl.textContent = `${r.total_files} files · last scanned ${fmtDate(r.last_scanned_at)}`;
      info.append(pathEl, metaEl);

      const status = document.createElement("span");
      status.className = "root-status" + (r.status === "scanning" ? " scanning" : "");
      status.textContent = r.status;

      const actions = document.createElement("div");
      actions.className = "root-actions";
      actions.append(
        button("secondary-btn", "Rescan", () => rescanRoot(r.root_path)),
        button("danger-btn", "Remove", () => removeRoot(r.root_path))
      );

      card.append(info, status, actions);
      list.appendChild(card);
    }
  }

  el("add-folder").addEventListener("click", async () => {
    const dir = await openDialog({
      directory: true,
      multiple: false,
      title: "Choose a sound library folder",
    });
    if (!dir) return;
    await invoke("add_root", { path: dir });
    await loadRoots();
  });

  async function rescanRoot(path) {
    await invoke("rescan_root", { path });
    await loadRoots();
  }

  async function removeRoot(path) {
    if (!confirm(`Remove "${path}" from your library? This only removes it from the index, not from disk.`)) return;
    await invoke("remove_root", { path });
    await loadRoots();
    await refreshFacets();
    runSearch(true);
  }

  // ---------------- Facets ----------------
  async function refreshFacets() {
    state.categories = await invoke("list_categories");
    state.soundTypes = await invoke("list_sound_types");
    state.folderTree = await invoke("list_folder_tree");
    const bounds = await invoke("get_duration_bounds");
    applyDurationBounds(bounds);
    renderCategories();
    renderSoundTypes();
    renderFolderTree();
  }

  function applyDurationBounds(bounds) {
    const minEl = el("min-secs");
    const maxEl = el("max-secs");
    const lo = Math.max(0, Math.floor(bounds.min));
    const hi = Math.max(lo + 1, Math.ceil(bounds.max));
    minEl.min = lo;
    minEl.max = hi;
    maxEl.min = lo;
    maxEl.max = hi;
    if (state.minSecs === null) state.minSecs = lo;
    if (state.maxSecs === null) state.maxSecs = hi;
    minEl.value = state.minSecs;
    maxEl.value = state.maxSecs;
    updateLengthSliderUI();
  }

  function updateLengthSliderUI() {
    const minEl = el("min-secs");
    const maxEl = el("max-secs");
    const lo = parseFloat(minEl.min);
    const hi = parseFloat(minEl.max);
    const range = hi - lo || 1;
    const minVal = parseFloat(minEl.value);
    const maxVal = parseFloat(maxEl.value);
    const leftPct = ((minVal - lo) / range) * 100;
    const rightPct = ((maxVal - lo) / range) * 100;
    el("slider-range-fill").style.left = `${leftPct}%`;
    el("slider-range-fill").style.width = `${Math.max(0, rightPct - leftPct)}%`;
    el("length-range-label").textContent = `${fmtDuration(minVal)} – ${fmtDuration(maxVal)}`;
  }

  function renderCategories() {
    const wrap = el("category-list");
    wrap.innerHTML = "";
    for (const c of state.categories) {
      const chip = document.createElement("div");
      chip.className = "chip" + (state.selectedCategories.has(c) ? " selected" : "");
      chip.textContent = c;
      chip.addEventListener("click", () => {
        if (state.selectedCategories.has(c)) state.selectedCategories.delete(c);
        else state.selectedCategories.add(c);
        renderCategories();
        runSearch(true);
      });
      wrap.appendChild(chip);
    }
  }

  el("clear-categories").addEventListener("click", () => {
    state.selectedCategories.clear();
    renderCategories();
    runSearch(true);
  });

  function renderSoundTypes() {
    const wrap = el("sound-type-list");
    wrap.innerHTML = "";
    for (const t of state.soundTypes) {
      const chip = document.createElement("div");
      chip.className = "chip" + (state.selectedSoundTypes.has(t) ? " selected" : "");
      chip.textContent = t;
      chip.addEventListener("click", () => {
        if (state.selectedSoundTypes.has(t)) state.selectedSoundTypes.delete(t);
        else state.selectedSoundTypes.add(t);
        renderSoundTypes();
        runSearch(true);
      });
      wrap.appendChild(chip);
    }
  }

  el("clear-sound-types").addEventListener("click", () => {
    state.selectedSoundTypes.clear();
    renderSoundTypes();
    runSearch(true);
  });

  el("favorites-only").addEventListener("change", (e) => {
    state.favoritesOnly = e.target.checked;
    runSearch(true);
  });

  function shortRootLabel(root) {
    const parts = root.split("/").filter(Boolean);
    return parts[parts.length - 1] || root;
  }

  function buildFolderTree(folderSet) {
    const tree = { children: {} };
    for (const f of folderSet) {
      if (!f) continue;
      const parts = f.split("/").filter(Boolean);
      let node = tree;
      let acc = "";
      for (const p of parts) {
        acc = acc ? `${acc}/${p}` : p;
        node.children = node.children || {};
        node.children[p] = node.children[p] || { path: acc, children: undefined };
        node = node.children[p];
      }
    }
    return tree;
  }

  // Keeps only branches whose name matches `query`, or that contain a
  // matching descendant; a matching branch keeps its full subtree.
  function pruneTreeForSearch(node, query) {
    if (!node.children) return node;
    const filtered = {};
    for (const [name, child] of Object.entries(node.children)) {
      const selfMatch = name.toLowerCase().includes(query);
      if (selfMatch) {
        filtered[name] = child;
        continue;
      }
      const prunedChild = pruneTreeForSearch(child, query);
      if (prunedChild.children && Object.keys(prunedChild.children).length > 0) {
        filtered[name] = prunedChild;
      }
    }
    return { ...node, children: filtered };
  }

  function isFolderSelected(root, folderPath) {
    if (!state.selectedFolder) return false;
    return state.selectedFolder.root === root && state.selectedFolder.folder === folderPath;
  }

  function renderBreadcrumb() {
    const wrap = el("folder-breadcrumb");
    wrap.innerHTML = "";
    if (!state.selectedFolder) {
      wrap.hidden = true;
      return;
    }
    wrap.hidden = false;
    const { root, folder } = state.selectedFolder;

    const segments = [{ label: shortRootLabel(root), path: null }];
    if (folder) {
      const parts = folder.split("/").filter(Boolean);
      let acc = "";
      for (const p of parts) {
        acc = acc ? `${acc}/${p}` : p;
        segments.push({ label: p, path: acc });
      }
    }

    segments.forEach((seg, i) => {
      if (i > 0) {
        const sep = document.createElement("span");
        sep.className = "breadcrumb-sep";
        sep.textContent = "›";
        wrap.appendChild(sep);
      }
      const el2 = document.createElement("span");
      el2.className = "breadcrumb-seg";
      el2.textContent = seg.label;
      if (i < segments.length - 1) {
        el2.addEventListener("click", () => selectFolder(root, seg.path));
      }
      wrap.appendChild(el2);
    });
  }

  function selectFolder(root, folderPath) {
    state.selectedFolder = isFolderSelected(root, folderPath) ? null : { root, folder: folderPath };
    renderBreadcrumb();
    renderFolderTree();
    runSearch(true);
  }

  function renderTreeLevel(root, node, forceExpand) {
    const container = document.createElement("div");
    container.className = "tree-children";
    if (!node.children) return container;
    const names = Object.keys(node.children).sort();
    for (const name of names) {
      const child = node.children[name];
      const hasChildren = !!(child.children && Object.keys(child.children).length > 0);
      const key = `${root}::${child.path}`;
      const collapsed = !forceExpand && state.collapsedFolders.has(key);

      const row = document.createElement("div");
      row.className = "tree-row";

      const toggle = document.createElement("span");
      toggle.className = "tree-toggle" + (hasChildren ? "" : " leaf");
      toggle.textContent = hasChildren ? (collapsed ? "▸" : "▾") : "";
      if (hasChildren) {
        toggle.addEventListener("click", (e) => {
          e.stopPropagation();
          if (state.collapsedFolders.has(key)) state.collapsedFolders.delete(key);
          else state.collapsedFolders.add(key);
          renderFolderTree();
        });
      }

      const label = document.createElement("div");
      label.className = "tree-node" + (isFolderSelected(root, child.path) ? " selected" : "");
      label.textContent = name;
      label.addEventListener("click", () => selectFolder(root, child.path));

      row.append(toggle, label);
      container.appendChild(row);

      if (hasChildren && !collapsed) {
        container.appendChild(renderTreeLevel(root, child, forceExpand));
      }
    }
    return container;
  }

  function renderFolderTree() {
    const wrap = el("folder-tree");
    wrap.innerHTML = "";
    const byRoot = new Map();
    for (const entry of state.folderTree) {
      if (!byRoot.has(entry.root_path)) byRoot.set(entry.root_path, new Set());
      byRoot.get(entry.root_path).add(entry.folder_path);
    }

    const query = state.folderSearchText.trim().toLowerCase();
    for (const [root, folders] of byRoot) {
      let tree = buildFolderTree(folders);
      const searching = query.length > 0;
      if (searching) {
        tree = pruneTreeForSearch(tree, query);
        const rootMatches = shortRootLabel(root).toLowerCase().includes(query);
        const hasMatches = tree.children && Object.keys(tree.children).length > 0;
        if (!rootMatches && !hasMatches) continue;
      }

      const rootNode = document.createElement("div");
      const rootRow = document.createElement("div");
      rootRow.className = "tree-row";
      const rootSpacer = document.createElement("span");
      rootSpacer.className = "tree-toggle leaf";
      const rootLabel = document.createElement("div");
      rootLabel.className = "tree-node" + (isFolderSelected(root, null) ? " selected" : "");
      rootLabel.textContent = shortRootLabel(root);
      rootLabel.title = root;
      rootLabel.addEventListener("click", () => selectFolder(root, null));
      rootRow.append(rootSpacer, rootLabel);
      rootNode.appendChild(rootRow);
      rootNode.appendChild(renderTreeLevel(root, tree, searching));
      wrap.appendChild(rootNode);
    }
  }

  el("folder-search").addEventListener("input", (e) => {
    state.folderSearchText = e.target.value;
    renderFolderTree();
  });

  // ---------------- Search ----------------
  let searchDebounce = null;
  el("search-text").addEventListener("input", (e) => {
    state.searchText = e.target.value;
    clearTimeout(searchDebounce);
    searchDebounce = setTimeout(() => runSearch(true), 250);
  });
  el("min-secs").addEventListener("input", (e) => {
    let v = parseFloat(e.target.value);
    const maxV = parseFloat(el("max-secs").value);
    if (v > maxV) {
      v = maxV;
      e.target.value = v;
    }
    state.minSecs = v;
    updateLengthSliderUI();
  });
  el("min-secs").addEventListener("change", () => runSearch(true));

  el("max-secs").addEventListener("input", (e) => {
    let v = parseFloat(e.target.value);
    const minV = parseFloat(el("min-secs").value);
    if (v < minV) {
      v = minV;
      e.target.value = v;
    }
    state.maxSecs = v;
    updateLengthSliderUI();
  });
  el("max-secs").addEventListener("change", () => runSearch(true));

  el("load-more").addEventListener("click", () => runSearch(false));

  async function runSearch(reset) {
    if (reset) {
      state.offset = 0;
      state.results = [];
    }
    const filters = {
      text: state.searchText || null,
      root_path: state.selectedFolder ? state.selectedFolder.root : null,
      folder_path: state.selectedFolder ? state.selectedFolder.folder : null,
      categories: state.selectedCategories.size ? Array.from(state.selectedCategories) : null,
      min_secs: state.minSecs,
      max_secs: state.maxSecs,
      favorites_only: state.favoritesOnly || null,
      sound_types: state.selectedSoundTypes.size ? Array.from(state.selectedSoundTypes) : null,
      limit: state.limit,
      offset: state.offset,
    };
    const rows = await invoke("search_files", { filters });
    state.results = reset ? rows : state.results.concat(rows);
    state.offset += rows.length;
    el("load-more").hidden = rows.length < state.limit;
    renderResults();
  }

  function fmtDuration(secs) {
    if (secs === null || secs === undefined) return "--:--";
    const s = Math.round(secs);
    const m = Math.floor(s / 60);
    const r = s % 60;
    return `${m}:${r.toString().padStart(2, "0")}`;
  }

  function renderResults() {
    el("results-meta").textContent = `${state.results.length} sound${state.results.length === 1 ? "" : "s"}`;
    const list = el("results-list");
    list.innerHTML = "";
    state.results.forEach((f) => {
      const row = document.createElement("div");
      row.className = "result-row" + (state.selectedFile && state.selectedFile.id === f.id ? " selected" : "");
      row.addEventListener("click", () => selectFile(f));

      const heart = document.createElement("button");
      heart.className = "row-heart" + (f.favorite ? " active" : "");
      heart.textContent = f.favorite ? "♥" : "♡";
      heart.title = "Favorite";
      heart.addEventListener("click", (e) => {
        e.stopPropagation();
        toggleFavorite(f);
      });

      const name = document.createElement("div");
      name.className = "result-name";
      name.textContent = f.filename;
      name.title = f.filename;

      const cat = document.createElement("div");
      cat.className = "result-category";
      cat.textContent = f.parent_folder || "";

      const dur = document.createElement("div");
      dur.className = "result-duration";
      dur.textContent = fmtDuration(f.duration_secs);

      const drag = document.createElement("div");
      drag.className = "result-drag";
      drag.textContent = "⠿";
      drag.title = "Drag into another app";
      drag.addEventListener("mousedown", (e) => {
        e.stopPropagation();
        beginDrag(f);
      });

      row.append(heart, name, cat, dur, drag);
      list.appendChild(row);
    });
  }

  async function toggleFavorite(f) {
    const newVal = await invoke("toggle_favorite", { id: f.id });
    f.favorite = newVal;
    renderResults();
    updatePlayerFavoriteBtn();
    if (state.favoritesOnly && !newVal) runSearch(true);
  }

  function addMeta(container, k, v) {
    const kEl = document.createElement("span");
    kEl.className = "k";
    kEl.textContent = k;
    const vEl = document.createElement("span");
    vEl.className = "v";
    vEl.textContent = v;
    container.append(kEl, vEl);
  }

  function updatePlayerFavoriteBtn() {
    const btn = el("player-favorite");
    const active = !!(state.selectedFile && state.selectedFile.favorite);
    btn.classList.toggle("active", active);
    btn.textContent = active ? "♥" : "♡";
  }
  el("player-favorite").addEventListener("click", () => {
    if (state.selectedFile) toggleFavorite(state.selectedFile);
  });

  function selectFile(f) {
    state.selectedFile = f;
    renderResults();
    el("player-empty").hidden = true;
    el("player-loaded").hidden = false;
    el("player-filename").textContent = f.filename;
    updatePlayerFavoriteBtn();

    const meta = el("player-meta");
    meta.innerHTML = "";
    addMeta(meta, "Category", f.parent_folder || "—");
    addMeta(meta, "Folder", f.folder_path || "—");
    addMeta(meta, "Duration", fmtDuration(f.duration_secs));
    addMeta(meta, "Library", shortRootLabel(f.root_path));
    if (f.dsp_tags) addMeta(meta, "Type", f.dsp_tags.split(",").filter(Boolean).join(", "));
    if (f.tags) addMeta(meta, "Tags", f.tags);
    el("player-description").textContent = f.description || "";

    loadWaveform(f.path);

    if (state.autoplay) {
      playSelected();
    } else {
      state.isPlaying = false;
      state.playStartedAt = null;
      updatePlayToggle();
    }
  }

  function beginDrag(f) {
    startDrag({ item: [f.path] }).catch((err) => console.error("drag failed", err));
  }
  el("drag-handle").addEventListener("mousedown", () => {
    if (state.selectedFile) beginDrag(state.selectedFile);
  });

  // ---------------- Waveform + playhead ("Mäusekino") ----------------
  let waveformAnimFrame = null;

  function resizeCanvasForDpr(canvas) {
    const dpr = window.devicePixelRatio || 1;
    const rect = canvas.getBoundingClientRect();
    const w = Math.max(1, Math.round(rect.width * dpr));
    const h = Math.max(1, Math.round(rect.height * dpr));
    if (canvas.width !== w || canvas.height !== h) {
      canvas.width = w;
      canvas.height = h;
    }
  }

  async function loadWaveform(path) {
    state.waveform = null;
    drawWaveform();
    try {
      const canvas = el("waveform-canvas");
      const buckets = Math.max(100, Math.floor(canvas.clientWidth || 300));
      const resp = await invoke("get_waveform", { path, buckets });
      state.waveform = resp.channels;
      drawWaveform();
    } catch (err) {
      console.warn("waveform unavailable for", path, err);
    }
  }

  function drawWaveform() {
    const canvas = el("waveform-canvas");
    resizeCanvasForDpr(canvas);
    const ctx = canvas.getContext("2d");
    const W = canvas.width;
    const H = canvas.height;
    ctx.clearRect(0, 0, W, H);
    if (!state.waveform || state.waveform.length === 0) return;

    const channels = state.waveform;
    const laneH = H / channels.length;
    ctx.strokeStyle = "#7c6cf0";
    channels.forEach((peaks, ci) => {
      if (peaks.length === 0) return;
      const midY = laneH * ci + laneH / 2;
      const scaleY = (laneH / 2) * 0.9;
      const stepX = W / peaks.length;
      ctx.lineWidth = Math.max(1, stepX);
      ctx.beginPath();
      for (let i = 0; i < peaks.length; i++) {
        const [mn, mx] = peaks[i];
        const x = i * stepX + stepX / 2;
        const yTop = midY - mx * scaleY;
        const yBot = midY - mn * scaleY;
        ctx.moveTo(x, yTop);
        ctx.lineTo(x, Math.max(yTop + 1, yBot));
      }
      ctx.stroke();
    });

    drawPlayhead(ctx, W, H);
  }

  function drawPlayhead(ctx, W, H) {
    if (!state.isPlaying || !state.selectedFile || state.playStartedAt === null) return;
    const duration = state.selectedFile.duration_secs || 0;
    if (duration <= 0) return;
    const elapsed = (performance.now() - state.playStartedAt) / 1000;
    const t = state.loopPlayback ? elapsed % duration : Math.min(elapsed, duration);
    const x = (t / duration) * W;
    ctx.strokeStyle = "#ffffff";
    ctx.lineWidth = Math.max(1, window.devicePixelRatio || 1);
    ctx.beginPath();
    ctx.moveTo(x, 0);
    ctx.lineTo(x, H);
    ctx.stroke();
  }

  function animatePlayhead() {
    drawWaveform();
    if (state.isPlaying) {
      invoke("get_playback_spectrum", { bars: METER_BARS })
        .then(updateLevelMeter)
        .catch(() => {});
      waveformAnimFrame = requestAnimationFrame(animatePlayhead);
    }
  }

  function stopVisualPlayback() {
    if (waveformAnimFrame) {
      cancelAnimationFrame(waveformAnimFrame);
      waveformAnimFrame = null;
    }
    drawWaveform();
    resetLevelMeter();
  }

  // ---------------- "Mäusekino" spectrum meter ----------------
  // A row of vertical bars, each one a log-spaced frequency band (low
  // frequencies on the left, high on the right) — a classic real-time
  // spectrum analyzer, not a level-over-time scroll.
  const METER_BARS = 40;
  let spectrum = new Array(METER_BARS).fill(0);

  function buildLevelMeter() {
    const wrap = el("level-meter");
    wrap.innerHTML = "";
    for (let i = 0; i < METER_BARS; i++) {
      const bar = document.createElement("div");
      bar.className = "bar";
      wrap.appendChild(bar);
    }
    resetLevelMeter();
  }

  function paintLevelMeter() {
    const bars = el("level-meter").children;
    for (let i = 0; i < bars.length; i++) {
      const v = spectrum[i];
      bars[i].style.height = `${Math.max(4, Math.round(v * 100))}%`;
      bars[i].className = "bar " + (v < 0.6 ? "green" : v < 0.85 ? "yellow" : "red");
    }
  }

  function updateLevelMeter(values) {
    spectrum = values;
    paintLevelMeter();
  }

  function resetLevelMeter() {
    spectrum = new Array(METER_BARS).fill(0);
    paintLevelMeter();
  }

  window.addEventListener("resize", () => {
    if (state.waveform) drawWaveform();
  });

  // ---------------- Playback ----------------
  function updatePlayToggle() {
    const btn = el("play-toggle");
    btn.textContent = state.isPlaying ? "⏹ Stop" : "▶ Play";
    btn.classList.toggle("playing", state.isPlaying);
  }

  function playSelected() {
    if (!state.selectedFile) return;
    invoke("play_file", { path: state.selectedFile.path, loopPlayback: state.loopPlayback });
    state.isPlaying = true;
    state.playStartedAt = performance.now();
    updatePlayToggle();
    animatePlayhead();
  }

  function stopPlayback() {
    invoke("stop_playback");
    state.isPlaying = false;
    state.playStartedAt = null;
    updatePlayToggle();
    stopVisualPlayback();
  }

  el("play-toggle").addEventListener("click", () => {
    if (state.isPlaying) stopPlayback();
    else playSelected();
  });

  el("waveform-canvas").addEventListener("click", async (e) => {
    const f = state.selectedFile;
    const duration = f && f.duration_secs;
    if (!f || !duration) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const frac = Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
    const secs = frac * duration;

    if (!state.isPlaying) {
      invoke("play_file", { path: f.path, loopPlayback: state.loopPlayback });
      state.isPlaying = true;
      updatePlayToggle();
      animatePlayhead();
    }
    await invoke("seek_playback", { secs }).catch(() => {});
    state.playStartedAt = performance.now() - secs * 1000;
    drawWaveform();
  });

  el("opt-autoplay").checked = state.autoplay;
  el("opt-loop").checked = state.loopPlayback;
  el("opt-autoplay").addEventListener("change", (e) => {
    state.autoplay = e.target.checked;
    savePref("fx.autoplay", state.autoplay);
  });
  el("opt-loop").addEventListener("change", (e) => {
    state.loopPlayback = e.target.checked;
    savePref("fx.loop", state.loopPlayback);
    if (state.isPlaying) playSelected(); // restart with the new loop setting
  });

  // ---------------- Resizable side panels ----------------
  function setupResize(handleEl, panelEl, { min, max, invert, prefKey }) {
    let dragging = false;
    let startX = 0;
    let startWidth = 0;

    handleEl.addEventListener("mousedown", (e) => {
      dragging = true;
      startX = e.clientX;
      startWidth = panelEl.getBoundingClientRect().width;
      handleEl.classList.add("dragging");
      document.body.style.cursor = "col-resize";
      e.preventDefault();
    });
    window.addEventListener("mousemove", (e) => {
      if (!dragging) return;
      const delta = invert ? startX - e.clientX : e.clientX - startX;
      const newWidth = Math.max(min, Math.min(max, startWidth + delta));
      panelEl.style.width = `${newWidth}px`;
      if (state.waveform) drawWaveform();
    });
    window.addEventListener("mouseup", () => {
      if (!dragging) return;
      dragging = false;
      handleEl.classList.remove("dragging");
      document.body.style.cursor = "";
      savePref(prefKey, parseInt(panelEl.style.width, 10));
    });
  }

  el("sidebar").style.width = `${loadPref("fx.sidebarWidth", 260)}px`;
  el("player-panel").style.width = `${loadPref("fx.playerWidth", 340)}px`;
  setupResize(document.querySelector('[data-resize="sidebar"]'), el("sidebar"), {
    min: 190,
    max: 360,
    invert: false,
    prefKey: "fx.sidebarWidth",
  });
  setupResize(document.querySelector('[data-resize="player"]'), el("player-panel"), {
    min: 240,
    max: 440,
    invert: true,
    prefKey: "fx.playerWidth",
  });

  function setupVerticalResize(handleEl, targetEl, { min, max, prefKey }) {
    let dragging = false;
    let startY = 0;
    let startHeight = 0;

    handleEl.addEventListener("mousedown", (e) => {
      dragging = true;
      startY = e.clientY;
      startHeight = targetEl.getBoundingClientRect().height;
      handleEl.classList.add("dragging");
      document.body.style.cursor = "row-resize";
      e.preventDefault();
    });
    window.addEventListener("mousemove", (e) => {
      if (!dragging) return;
      const newHeight = Math.max(min, Math.min(max, startHeight + (e.clientY - startY)));
      targetEl.style.height = `${newHeight}px`;
    });
    window.addEventListener("mouseup", () => {
      if (!dragging) return;
      dragging = false;
      handleEl.classList.remove("dragging");
      document.body.style.cursor = "";
      savePref(prefKey, parseInt(targetEl.style.height, 10));
    });
  }

  el("category-list").style.height = `${loadPref("fx.categoriesHeight", 140)}px`;
  el("sound-type-list").style.height = `${loadPref("fx.soundTypesHeight", 90)}px`;
  setupVerticalResize(document.querySelector('[data-resize-v="categories"]'), el("category-list"), {
    min: 32,
    max: 400,
    prefKey: "fx.categoriesHeight",
  });
  setupVerticalResize(document.querySelector('[data-resize-v="soundtypes"]'), el("sound-type-list"), {
    min: 32,
    max: 400,
    prefKey: "fx.soundTypesHeight",
  });

  // ---------------- Keyboard shortcuts ----------------
  function moveSelection(delta) {
    if (state.results.length === 0) return;
    let idx = state.selectedFile ? state.results.findIndex((f) => f.id === state.selectedFile.id) : -1;
    idx = Math.max(0, Math.min(state.results.length - 1, idx + delta));
    selectFile(state.results[idx]);
    const rows = document.querySelectorAll(".result-row");
    if (rows[idx]) rows[idx].scrollIntoView({ block: "nearest" });
  }

  document.addEventListener("keydown", (e) => {
    if (state.view !== "browse") return;
    const active = document.activeElement;
    const tag = active ? active.tagName : "";
    const isNumberInput = tag === "INPUT" && active.type === "number";
    const isCheckbox = tag === "INPUT" && active.type === "checkbox";

    if (e.key === "Escape") {
      const input = el("search-text");
      if (input.value) {
        e.preventDefault();
        input.value = "";
        state.searchText = "";
        clearTimeout(searchDebounce);
        runSearch(true);
      }
      input.blur();
      return;
    }

    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      if (isNumberInput) return; // let the native spinner increment/decrement
      e.preventDefault();
      moveSelection(e.key === "ArrowDown" ? 1 : -1);
      return;
    }

    // Buttons/checkboxes/selects: let their own native key handling run.
    if (tag === "BUTTON" || tag === "SELECT" || isCheckbox) return;

    const isTextInput = tag === "INPUT" || tag === "TEXTAREA";
    if (isTextInput) return; // let normal typing (including space) happen

    if (e.key === " ") {
      e.preventDefault();
      if (state.isPlaying) stopPlayback();
      else playSelected();
      return;
    }

    if (e.key.length === 1 && !e.ctrlKey && !e.altKey && !e.metaKey) {
      const input = el("search-text");
      input.focus();
      input.value += e.key;
      state.searchText = input.value;
      clearTimeout(searchDebounce);
      searchDebounce = setTimeout(() => runSearch(true), 250);
    }
  });

  // ---------------- Footer / progress ----------------
  function setScanning(rootPath, payload) {
    const wasEmpty = state.scanning.size === 0;
    state.scanning.set(rootPath, payload);
    if (wasEmpty) el("footer").classList.add("expanded");
    updateFooter();
  }

  function clearScanning(rootPath) {
    state.scanning.delete(rootPath);
    if (state.scanning.size === 0) el("footer").classList.remove("expanded");
    updateFooter();
  }

  function updateFooter() {
    const summary = el("footer-summary");
    if (state.scanning.size === 0) {
      summary.textContent = "Idle";
      el("footer-progress-fill").style.width = "0%";
      el("footer-current-file").textContent = "";
      return;
    }
    let processed = 0,
      total = 0,
      currentFile = "";
    for (const p of state.scanning.values()) {
      processed += p.processed;
      total += p.total;
      currentFile = p.current_file || currentFile;
    }
    summary.textContent = `Indexing… ${processed}/${total} files`;
    el("footer-progress-fill").style.width = total ? `${Math.min(100, (processed / total) * 100)}%` : "0%";
    el("footer-current-file").textContent = currentFile;
  }

  el("footer-toggle").addEventListener("click", () => {
    el("footer").classList.toggle("expanded");
  });

  // ---------------- Backend events ----------------
  listen("index-progress", (ev) => setScanning(ev.payload.root_path, ev.payload));
  listen("index-complete", async (ev) => {
    clearScanning(ev.payload.root_path);
    await loadRoots();
    if (state.view === "browse") {
      await refreshFacets();
      runSearch(true);
    }
  });

  // ---------------- Init ----------------
  (async function init() {
    buildLevelMeter();
    await loadRoots();
    switchView(state.roots.length === 0 ? "settings" : "browse");
  })();
})();
