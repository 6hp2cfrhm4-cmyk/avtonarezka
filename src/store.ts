import { create } from "zustand";
import type { AppInfo, Asset, Defaults, Draft, Moment, Post, Project, PublishJob, QueueState, TikTokStatus, YoutubeProgressEvent } from "./lib/api";

export type ScreenId = "import" | "analysis" | "moments" | "drafts" | "final";

export const SCREENS: { id: ScreenId; label: string; hint: string }[] = [
  { id: "import", label: "Импорт", hint: "Видео и ассеты" },
  { id: "analysis", label: "Анализ", hint: "Речь, звук, сцены" },
  { id: "moments", label: "Моменты", hint: "Отбор лучшего" },
  { id: "drafts", label: "Черновики", hint: "Проверка и правки" },
  { id: "final", label: "Финал", hint: "Рендер и TikTok" },
];

export const ACCENTS = ["#b6ff3c", "#8b6cff", "#35e0ff", "#ff4d8d", "#ffb020"];

export interface AnalysisState {
  running: boolean;
  stage: string;
  fraction: number;
  message: string;
  notes: string[];
}

type Toast = { id: number; text: string; kind: "info" | "error"; count: number };

/** Загрузка с YouTube: последний снимок от бэкенда плюс итог. */
export interface DownloadJob extends YoutubeProgressEvent {
  status: "active" | "done" | "error" | "stopped";
  error: string | null;
  project: Project | null;
}

interface UiState {
  screen: ScreenId;
  dir: 1 | -1;
  project: Project | null;
  projects: Project[];
  info: AppInfo | null;
  accent: string;
  lite: boolean;
  toasts: Toast[];
  analysis: AnalysisState;
  moments: Moment[];
  assets: Asset[];
  defaults: Defaults | null;
  drafts: Draft[];
  /// Прогресс рендера по id черновика, 0..1.
  renderProgress: Record<number, number>;
  tiktok: TikTokStatus | null;
  posts: Record<number, Post>;
  /** Ход отправки в TikTok по id черновика. */
  postProgress: Record<number, { fraction: number; message: string }>;
  /** Очередь автовыкладки выбранного проекта и её задачи по id черновика (карточки берут свою без перерисовки остальных). */
  queue: QueueState | null;
  queueJobs: Record<number, PublishJob>;
  /** Что сейчас делает модель по черновику: «Пишу описание, часть 2 из 4». */
  queueText: Record<number, string>;
  setQueue: (q: QueueState | null) => void;
  setQueueText: (draftId: number, message: string | null) => void;
  /** Загрузки с YouTube по id задачи (в порядке появления). */
  downloads: Record<string, DownloadJob>;
  upsertDownload: (e: YoutubeProgressEvent) => void;
  patchDownload: (id: string, patch: Partial<DownloadJob>) => void;
  removeDownload: (id: string) => void;
  setTikTok: (t: TikTokStatus) => void;
  setPosts: (p: Post[]) => void;
  setPostProgress: (id: number, p: { fraction: number; message: string } | null) => void;
  setAssets: (a: Asset[]) => void;
  setDefaults: (d: Defaults) => void;
  setDrafts: (d: Draft[]) => void;
  patchDraft: (d: Draft) => void;
  setRenderProgress: (id: number, f: number | null) => void;
  setAnalysis: (a: Partial<AnalysisState>) => void;
  setMoments: (m: Moment[]) => void;
  patchMoment: (m: Moment) => void;
  go: (s: ScreenId) => void;
  setProject: (p: Project | null) => void;
  setProjects: (p: Project[]) => void;
  setInfo: (i: AppInfo) => void;
  setAccent: (c: string) => void;
  setLite: (v: boolean) => void;
  notify: (text: string, kind?: Toast["kind"]) => void;
}

const read = (k: string): string | null => {
  try {
    return localStorage.getItem(k);
  } catch {
    return null;
  }
};
const write = (k: string, v: string) => {
  try {
    localStorage.setItem(k, v);
  } catch {
    /* хранилище недоступно — настройки просто не сохранятся */
  }
};

const order = (s: ScreenId) => SCREENS.findIndex((x) => x.id === s);
let toastId = 0;
const toastTimers = new Map<number, ReturnType<typeof setTimeout>>();
/** Одновременно на экране не больше стольких уведомлений: старые уходят первыми. */
const MAX_TOASTS = 4;

/** Тот же драфт без изменений: прежний объект, чтобы `memo`-карточки не перерисовывались при каждом перечитывании списка. */
function sameDraft(a: Draft, b: Draft): boolean {
  return (
    a.id === b.id && a.status === b.status && a.error === b.error && a.updatedAt === b.updatedAt && a.part === b.part &&
    a.title === b.title && a.start === b.start && a.end === b.end && a.climax === b.climax &&
    a.proxyPath === b.proxyPath && a.posterPath === b.posterPath && a.finalPath === b.finalPath &&
    JSON.stringify(a.config) === JSON.stringify(b.config)
  );
}

export const useUi = create<UiState>((set, get) => ({
  screen: "import",
  dir: 1,
  project: null,
  projects: [],
  info: null,
  accent: read("accent") ?? ACCENTS[0],
  lite: read("lite") === "1",
  toasts: [],
  analysis: { running: false, stage: "", fraction: 0, message: "", notes: [] },
  moments: [],
  assets: [],
  defaults: null,
  drafts: [],
  renderProgress: {},
  tiktok: null,
  posts: {},
  postProgress: {},
  queue: null,
  queueJobs: {},
  queueText: {},
  setQueue: (queue) => set({ queue, queueJobs: Object.fromEntries((queue?.jobs ?? []).map((j) => [j.draftId, j])) }),
  setQueueText: (id, message) =>
    set((s) => {
      const next = { ...s.queueText };
      if (message === null) delete next[id];
      else next[id] = message;
      return { queueText: next };
    }),
  downloads: {},
  upsertDownload: (e) =>
    set((s) => {
      const old = s.downloads[e.jobId];
      // запоздавший снимок не должен «оживить» уже законченную загрузку
      if (old && old.status !== "active") return s;
      return { downloads: { ...s.downloads, [e.jobId]: { ...e, status: "active", error: null, project: null } } };
    }),
  patchDownload: (id, patch) =>
    set((s) => (s.downloads[id] ? { downloads: { ...s.downloads, [id]: { ...s.downloads[id], ...patch } } } : s)),
  removeDownload: (id) =>
    set((s) => {
      const next = { ...s.downloads };
      delete next[id];
      return { downloads: next };
    }),
  setTikTok: (tiktok) => set({ tiktok }),
  setPosts: (posts) => set({ posts: Object.fromEntries(posts.map((p) => [p.draftId, p])) }),
  setPostProgress: (id, p) =>
    set((s) => {
      const next = { ...s.postProgress };
      if (p === null) delete next[id];
      else next[id] = p;
      return { postProgress: next };
    }),
  setAssets: (assets) => set({ assets }),
  setDefaults: (defaults) => set({ defaults }),
  setDrafts: (next) =>
    set((s) => {
      const old = new Map(s.drafts.map((d) => [d.id, d]));
      return { drafts: next.map((d) => { const o = old.get(d.id); return o && sameDraft(o, d) ? o : d; }) };
    }),
  patchDraft: (d) => set((s) => ({ drafts: s.drafts.map((x) => (x.id === d.id ? d : x)) })),
  setRenderProgress: (id, f) =>
    set((s) => {
      const next = { ...s.renderProgress };
      if (f === null) delete next[id];
      else next[id] = f;
      return { renderProgress: next };
    }),
  setAnalysis: (a) => set((s) => ({ analysis: { ...s.analysis, ...a } })),
  setMoments: (moments) => set({ moments }),
  patchMoment: (m) => set((s) => ({ moments: s.moments.map((x) => (x.id === m.id ? m : x)) })),
  go: (s) => set({ screen: s, dir: order(s) >= order(get().screen) ? 1 : -1 }),
  setProject: (project) => set({ project }),
  setProjects: (projects) => set({ projects }),
  setInfo: (info) => set({ info }),
  setAccent: (accent) => {
    write("accent", accent);
    set({ accent });
  },
  setLite: (lite) => {
    write("lite", lite ? "1" : "0");
    set({ lite });
  },
  notify: (text, kind = "info") => {
    // одинаковые уведомления складываются в одно со счётчиком: сто сбоев рендера не должны давать сто плашек
    const same = get().toasts.find((t) => t.text === text && t.kind === kind);
    const id = same?.id ?? ++toastId;
    clearTimeout(toastTimers.get(id));
    set((s) => ({
      toasts: same
        ? s.toasts.map((t) => (t.id === id ? { ...t, count: t.count + 1 } : t))
        : [...s.toasts, { id, text, kind, count: 1 }].slice(-MAX_TOASTS),
    }));
    toastTimers.set(id, setTimeout(() => {
      toastTimers.delete(id);
      set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }));
    }, 5200));
  },
}));

/** Насколько далеко пользователь может уйти по шагам для текущего проекта. */
export function maxStep(project: Project | null): number {
  if (!project) return 0;
  switch (project.status) {
    case "imported":
      return 1;
    case "analyzed":
      return 2;
    case "drafts":
      return 3;
    default:
      return 4;
  }
}
