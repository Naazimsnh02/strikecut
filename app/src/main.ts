import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { confirm, open, save } from "@tauri-apps/plugin-dialog";

// Mirrors strikecut-core's serialized types.
type Word = { text: string; start: number; end: number; speaker?: number; deleted?: boolean };
type CutOptions = { padding: number; max_pause: number | null; min_span: number };
type Project = {
  version: number;
  media: string;
  duration: number;
  has_video: boolean;
  cut: CutOptions;
  transcript: { language?: string; words: Word[] };
};
type Span = { start: number; end: number };
type Cut = { spans: Span[]; duration: number };
type ModelStatus = { path: string; exists: boolean };

const MEDIA_EXTENSIONS = ["mp4", "mov", "mkv", "webm", "m4v", "avi", "mp3", "wav", "m4a", "aac", "flac", "ogg", "opus"];
/** Start a new paragraph after a pause this long (seconds) or a speaker change. */
const PARAGRAPH_PAUSE = 1.5;

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const ui = {
  open: $<HTMLButtonElement>("open"),
  openEmpty: $<HTMLButtonElement>("open-empty"),
  fillers: $<HTMLButtonElement>("fillers"),
  pauses: $<HTMLSelectElement>("pauses"),
  undo: $<HTMLButtonElement>("undo"),
  length: $("length"),
  srt: $<HTMLInputElement>("srt"),
  export: $<HTMLButtonElement>("export"),
  workspace: $("workspace"),
  empty: $("empty"),
  video: $<HTMLVideoElement>("video"),
  play: $<HTMLButtonElement>("play"),
  clock: $("clock"),
  preview: $<HTMLInputElement>("preview"),
  transcript: $("transcript"),
  busy: $("busy"),
  busyText: $("busy-text"),
  busyProgress: $<HTMLProgressElement>("busy-progress"),
  toast: $("toast"),
};

let project: Project | null = null;
let cut: Cut = { spans: [], duration: 0 };
let wordEls: HTMLSpanElement[] = [];
let currentWord = -1;
/** Undo stack: snapshots of which words were deleted, plus the pause setting. */
const history: { deleted: boolean[]; maxPause: number | null }[] = [];

// ---------- helpers ----------

function fmt(secs: number): string {
  const s = Math.max(0, Math.round(secs));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

let toastTimer: ReturnType<typeof setTimeout> | undefined;

function toast(msg: string, ms = 3500) {
  ui.toast.textContent = msg;
  ui.toast.classList.remove("hidden");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => ui.toast.classList.add("hidden"), ms);
}

function busy(text: string | null, progress?: number) {
  ui.busy.classList.toggle("hidden", text === null);
  if (text !== null) ui.busyText.textContent = text;
  ui.busyProgress.classList.toggle("hidden", progress === undefined);
  if (progress !== undefined) ui.busyProgress.value = progress;
}

/** Index of the last element whose key is <= t (binary search), or -1. */
function lastAtOrBefore<T>(items: T[], t: number, key: (x: T) => number): number {
  let lo = 0;
  let hi = items.length - 1;
  let ans = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (key(items[mid]) <= t) {
      ans = mid;
      lo = mid + 1;
    } else hi = mid - 1;
  }
  return ans;
}

function sourceToOutput(t: number): number {
  let offset = 0;
  for (const s of cut.spans) {
    if (t < s.start) return offset;
    if (t <= s.end) return offset + (t - s.start);
    offset += s.end - s.start;
  }
  return offset;
}

// ---------- opening ----------

async function openFile() {
  const media = await open({ multiple: false, filters: [{ name: "Video or audio", extensions: MEDIA_EXTENSIONS }] });
  if (!media) return;

  try {
    let p = await invoke<Project | null>("open_media", { media });
    if (!p) p = await transcribeOrImport(media);
    if (p) await load(p);
  } catch (e) {
    toast(String(e), 8000);
  } finally {
    busy(null);
  }
}

async function transcribeOrImport(media: string): Promise<Project | null> {
  const model = await invoke<ModelStatus>("model_status");
  if (model.exists) {
    busy("Transcribing on your machine… long recordings can take a few minutes.");
    try {
      return await invoke<Project>("transcribe_media", { media, model: null, language: null });
    } catch (e) {
      busy(null);
      if (!(await confirm(`Transcription failed:\n${e}\n\nImport a transcript file instead?`, { title: "Strikecut", kind: "error" })))
        return null;
    }
  } else if (
    !(await confirm(
      `No speech model found at:\n${model.path}\n\nDownload ggml-base.en.bin from huggingface.co/ggerganov/whisper.cpp into that folder, ` +
        `or import a word-timestamped transcript (whisper.cpp, WhisperX, faster-whisper, OpenAI JSON).\n\nImport a transcript now?`,
      { title: "Speech model needed", okLabel: "Import transcript…", cancelLabel: "Cancel" },
    ))
  ) {
    return null;
  }

  const transcript = await open({ multiple: false, filters: [{ name: "Transcript JSON", extensions: ["json"] }] });
  if (!transcript) return null;
  busy("Importing transcript…");
  return await invoke<Project>("import_transcript", { media, transcript });
}

async function load(p: Project) {
  project = p;
  history.length = 0;
  ui.video.src = convertFileSrc(p.media);
  ui.video.classList.toggle("audio-only", !p.has_video);
  ui.pauses.value = p.cut.max_pause === null ? "" : p.cut.max_pause.toFixed(1);
  if (ui.pauses.selectedIndex < 0) ui.pauses.value = "";
  renderTranscript();
  ui.empty.classList.add("hidden");
  ui.workspace.classList.remove("hidden");
  for (const b of [ui.fillers, ui.pauses, ui.export]) b.disabled = false;
  document.title = `${p.media.split(/[\\/]/).pop()} — Strikecut`;
  await refresh(false);
  ui.transcript.focus();
}

// ---------- transcript ----------

function renderTranscript() {
  const words = project!.transcript.words;
  ui.transcript.replaceChildren();
  wordEls = [];
  let para = document.createElement("p");
  words.forEach((w, i) => {
    const prev = words[i - 1];
    if (prev && (w.start - prev.end > PARAGRAPH_PAUSE || w.speaker !== prev.speaker)) {
      ui.transcript.append(para);
      para = document.createElement("p");
    }
    const el = document.createElement("span");
    el.className = "w";
    el.dataset.i = String(i);
    el.textContent = w.text.trim();
    para.append(el, " ");
    wordEls.push(el);
  });
  ui.transcript.append(para);
}

function selectedWordIndices(): number[] {
  const sel = getSelection();
  if (!sel || sel.isCollapsed || !ui.transcript.contains(sel.anchorNode)) return [];
  const out: number[] = [];
  wordEls.forEach((el, i) => {
    if (sel.containsNode(el, true)) out.push(i);
  });
  return out;
}

function snapshot() {
  history.push({
    deleted: project!.transcript.words.map((w) => !!w.deleted),
    maxPause: project!.cut.max_pause,
  });
  if (history.length > 200) history.shift();
}

function toggleSelection() {
  const idx = selectedWordIndices();
  if (!idx.length) return;
  const words = project!.transcript.words;
  const restore = idx.every((i) => words[i].deleted);
  snapshot();
  for (const i of idx) words[i].deleted = !restore;
  getSelection()?.removeAllRanges();
  refresh();
}

function undo() {
  const h = history.pop();
  if (!h || !project) return;
  project.transcript.words.forEach((w, i) => (w.deleted = h.deleted[i]));
  project.cut.max_pause = h.maxPause;
  ui.pauses.value = h.maxPause === null ? "" : h.maxPause.toFixed(1);
  refresh();
}

let saveTimer: ReturnType<typeof setTimeout> | undefined;

/** Re-apply edit state to the UI and recompute the cut list. */
async function refresh(persist = true) {
  if (!project) return;
  const words = project.transcript.words;
  wordEls.forEach((el, i) => el.classList.toggle("del", !!words[i].deleted));
  ui.undo.disabled = history.length === 0;

  cut = await invoke<Cut>("keep_spans", { project });
  ui.length.innerHTML =
    cut.duration < project.duration - 0.05
      ? `${fmt(project.duration)} → <b>${fmt(cut.duration)}</b> <span class="saved">−${fmt(project.duration - cut.duration)}</span>`
      : fmt(project.duration);

  if (persist) {
    clearTimeout(saveTimer);
    const snapshotProject = project;
    saveTimer = setTimeout(() => invoke("save_project", { project: snapshotProject }).catch((e) => toast(`Save failed: ${e}`)), 400);
  }
}

// ---------- playback ----------

function tick() {
  if (!project) return;
  const v = ui.video;
  let t = v.currentTime;

  // Skip over cut regions while previewing the edit.
  if (ui.preview.checked && !v.paused && cut.spans.length) {
    const i = lastAtOrBefore(cut.spans, t, (s) => s.start);
    const inside = i >= 0 && t < cut.spans[i].end;
    if (!inside) {
      const next = cut.spans[i + 1];
      if (next) {
        v.currentTime = t = next.start;
      } else {
        v.pause();
      }
    }
  }

  const w = lastAtOrBefore(project.transcript.words, t, (x) => x.start);
  if (w !== currentWord) {
    wordEls[currentWord]?.classList.remove("now");
    currentWord = w;
    const el = wordEls[w];
    if (el) {
      el.classList.add("now");
      if (!v.paused) el.scrollIntoView({ block: "nearest", behavior: "smooth" });
    }
  }
  ui.clock.textContent = ui.preview.checked
    ? `${fmt(sourceToOutput(t))} / ${fmt(cut.duration)}`
    : `${fmt(t)} / ${fmt(project.duration)}`;

  if (!v.paused) requestAnimationFrame(tick);
}

function togglePlay() {
  if (!project) return;
  const v = ui.video;
  if (v.paused) {
    // Starting inside a cut region (or at the very end) jumps to the next kept part.
    if (ui.preview.checked && cut.spans.length) {
      const i = lastAtOrBefore(cut.spans, v.currentTime, (s) => s.start);
      if (i < 0 || v.currentTime >= cut.spans[i].end) {
        const next = cut.spans[i + 1] ?? cut.spans[0];
        v.currentTime = next.start;
      }
    }
    v.play();
  } else v.pause();
}

ui.video.addEventListener("play", () => {
  ui.play.textContent = "❚❚";
  requestAnimationFrame(tick);
});
ui.video.addEventListener("pause", () => (ui.play.textContent = "▶"));
ui.video.addEventListener("seeked", tick);

// ---------- events ----------

ui.open.onclick = openFile;
ui.openEmpty.onclick = openFile;
ui.play.onclick = togglePlay;
ui.undo.onclick = undo;
ui.preview.onchange = () => tick();

ui.fillers.onclick = async () => {
  if (!project) return;
  snapshot();
  const [p, n] = await invoke<[Project, number]>("remove_fillers", { project });
  if (n === 0) history.pop();
  project = p;
  refresh();
  toast(n ? `Removed ${n} filler word${n === 1 ? "" : "s"}` : "No filler words found");
};

ui.pauses.onchange = () => {
  if (!project) return;
  snapshot();
  project.cut.max_pause = ui.pauses.value ? Number(ui.pauses.value) : null;
  refresh();
};

ui.transcript.addEventListener("click", (e) => {
  const el = (e.target as HTMLElement).closest<HTMLElement>(".w");
  if (!el || !project || !getSelection()?.isCollapsed) return;
  ui.video.currentTime = project.transcript.words[Number(el.dataset.i)].start;
});

ui.export.onclick = async () => {
  if (!project) return;
  const src = project.media;
  const dot = src.lastIndexOf(".");
  const output = await save({
    defaultPath: dot > 0 ? `${src.slice(0, dot)}.edited${src.slice(dot)}` : `${src}.edited.mp4`,
    filters: [{ name: "Media", extensions: project.has_video ? ["mp4", "mov", "mkv", "webm"] : ["m4a", "mp3", "wav", "flac"] }],
  });
  if (!output) return;
  ui.video.pause();
  busy("Exporting…", 0);
  const unlisten = await listen<number>("export-progress", (e) => busy(`Exporting… ${Math.round(e.payload * 100)}%`, e.payload));
  try {
    const out = await invoke<string>("export_project", { project, output, srt: ui.srt.checked });
    toast(`Exported ${out.split(/[\\/]/).pop()}`, 6000);
  } catch (e) {
    toast(`Export failed: ${e}`, 10000);
  } finally {
    unlisten();
    busy(null);
  }
};

document.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "o") {
    e.preventDefault();
    openFile();
    return;
  }
  const tag = (e.target as HTMLElement).tagName;
  if (tag === "INPUT" || tag === "SELECT" || !project) return;
  if (e.key === "Delete" || e.key === "Backspace") {
    e.preventDefault();
    toggleSelection();
  } else if (e.key === " ") {
    e.preventDefault();
    togglePlay();
  } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "z") {
    e.preventDefault();
    undo();
  }
});
