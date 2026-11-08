import { invoke } from "@tauri-apps/api/core";
import { extractYoutubeLink } from "./youtube";

export interface EnvReport {
  /** `linux` или `windows`: по нему выбираются подсказки по установке утилит. */
  os: string;
  ffmpeg: string | null;
  nvenc: boolean;
  libass: boolean;
  /** Версия yt-dlp; null — программы нет. */
  ytdlp: string | null;
  /** Версия не старее той, с которой работает утилита-образец. */
  ytdlpOk: boolean;
  /** deno или node: без них YouTube отдаёт неполный список форматов. */
  jsRuntime: string | null;
}

export interface AppInfo {
  version: string;
  data_dir: string;
  env: EnvReport;
}

export interface Project {
  id: number;
  path: string;
  title: string;
  duration: number;
  width: number;
  height: number;
  fps: number;
  has_audio: boolean;
  status: string;
  created_at: number;
  /** Видео уже размечено: анализ режет его целиком подряд, без пропусков. */
  tight: boolean;
}

export interface ModelsStatus {
  dir: string;
  whisper: boolean;
  yamnet: boolean;
  llm: boolean;
  worker: boolean;
}

export interface AnalysisOptions {
  useEvents?: boolean;
  useSpeech?: boolean;
  useLlm?: boolean;
  minLen?: number;
  maxLen?: number;
  topK?: number;
  gpuLayers?: number;
}

export type MomentStatus = "candidate" | "accepted" | "rejected";

export interface Moment {
  id: number;
  projectId: number;
  start: number;
  end: number;
  score: number;
  signalScore: number;
  llmScore: number;
  title: string;
  caption: string;
  hashtags: string[];
  climax: number;
  transcript: string;
  status: MomentStatus;
  manual: boolean;
}

export interface ProgressEvent {
  projectId: number;
  stage: string;
  fraction: number;
  message: string;
}
export interface DoneEvent {
  projectId: number;
  moments: number;
  notes: string[];
}
export interface ErrorEvent {
  projectId: number;
  message: string;
  cancelled: boolean;
}


export type AssetKind = "banner" | "background" | "music";

export interface Asset {
  id: number;
  kind: AssetKind;
  path: string;
  name: string;
  duration: number;
  width: number;
  height: number;
  hasAudio: boolean;
  thumb: string | null;
}

/** `auto`: горизонтальный исходник (16:9) виден целиком, вертикальный заполняет область. */
export type TopFit = "auto" | "fill" | "contain";
export type BannerAnim = "none" | "fade" | "slideUp" | "pop";

export interface BannerStyle {
  enabled: boolean;
  keyColor: string;
  similarity: number;
  blend: number;
  despill: boolean;
  widthPct: number;
  offsetX: number;
  offsetY: number;
  anim: BannerAnim;
  maxSeconds: number | null;
}
export interface SubtitleStyle {
  enabled: boolean;
  font: string;
  size: number;
  color: string;
  highlight: string;
  outlineColor: string;
  outline: number;
  uppercase: boolean;
  wordsPerLine: number;
  highlightScale: number;
  yPct: number | null;
}
export interface PartLabelStyle {
  enabled: boolean;
  template: string;
  font: string;
  size: number;
  color: string;
  outlineColor: string;
  outline: number;
  yPct: number;
}
export interface AudioMix {
  main: number;
  banner: number;
  gameplay: number;
  music: number;
  loudnorm: boolean;
}
export interface Style {
  split: number;
  topFit: TopFit;
  /** Доля высоты исходника, срезаемая снизу (вшитые в видео субтитры). */
  topCropBottom: number;
  gameplayFreeze: boolean;
  banner: BannerStyle;
  subtitles: SubtitleStyle;
  partLabel: PartLabelStyle;
  audio: AudioMix;
}
export interface DraftConfig {
  style: Style;
  bannerId: number | null;
  backgroundId: number | null;
  gameplayOffset: number;
  musicId: number | null;
  musicOffset: number;
}
export interface Defaults {
  style: Style;
  bannerId: number | null;
  /** Фоны по порядку: лента геймплея идёт по ним, потом по кругу. */
  backgroundIds: number[];
  musicId: number | null;
  /** Версия раскладки умолчаний; ведёт и обновляет сам бэкенд. */
  layoutV?: number;
}
/** Докуда дошла лента геймплея: фон и секунда в нём. */
export interface GameplayTape {
  assetId: number;
  pos: number;
}
export type DraftStatus =
  | "queued" | "rendering" | "ready" | "approved" | "rejected" | "failed"
  | "final_queued" | "final_rendering" | "final_done" | "final_failed";
export interface Draft {
  id: number;
  momentId: number;
  projectId: number;
  part: number;
  title: string;
  caption: string;
  hashtags: string[];
  start: number;
  end: number;
  climax: number;
  status: DraftStatus;
  proxyPath: string | null;
  posterPath: string | null;
  finalPath: string | null;
  error: string | null;
  updatedAt: number;
  config: DraftConfig;
}
export type RenderKind = "draft" | "final";
export interface RenderProgressEvent {
  draftId: number;
  kind: RenderKind;
  fraction: number;
}
export interface RenderDoneEvent {
  draftId: number;
  kind: RenderKind;
}
export interface RenderFailedEvent {
  draftId: number;
  kind: RenderKind;
  message: string;
  cancelled: boolean;
}

export const defaultStyle: Style = {
  split: 0.5,
  topFit: "auto",
  topCropBottom: 0,
  gameplayFreeze: false,
  banner: { enabled: true, keyColor: "00ff00", similarity: 0.18, blend: 0.08, despill: true, widthPct: 0.72, offsetX: 0, offsetY: 0, anim: "pop", maxSeconds: null },
  subtitles: { enabled: true, font: "Montserrat ExtraBold", size: 84, color: "ffffff", highlight: "b6ff3c", outlineColor: "000000", outline: 7, uppercase: true, wordsPerLine: 3, highlightScale: 112, yPct: null },
  partLabel: { enabled: true, template: "Часть {n}", font: "Montserrat Black", size: 76, color: "ffffff", outlineColor: "000000", outline: 6, yPct: 0.11 },
  audio: { main: 1, banner: 1, gameplay: 0, music: 0.18, loudnorm: true },
};


export type TikTokMode = "inbox" | "direct";
export type TikTokMethod = "api" | "browser";
export type Privacy = "SELF_ONLY" | "MUTUAL_FOLLOW_FRIENDS" | "FOLLOWER_OF_CREATOR" | "PUBLIC_TO_EVERYONE";

export interface TikTokStatus {
  clientKey: string;
  port: number;
  mode: TikTokMode;
  method: TikTokMethod;
  privacy: Privacy;
  autoPublish: boolean;
  hasSecret: boolean;
  redirectUri: string;
  connected: boolean;
  refreshExpiresAt: number | null;
  scope: string;
  secretsBackend: "keyring" | "memory";
  chromium: boolean;
  loginInProgress: boolean;
}
export interface TikTokSettings {
  clientKey: string;
  /** Пусто или не задано — оставить прежний секрет. */
  clientSecret?: string;
  port: number;
  mode: TikTokMode;
  method: TikTokMethod;
  privacy: Privacy;
  autoPublish: boolean;
}
export interface PublishArgs {
  draftId: number;
  caption: string;
  hashtags: string[];
  privacy: Privacy;
  allowComment: boolean;
  allowDuet: boolean;
  allowStitch: boolean;
  force: boolean;
}
export interface PublishStart {
  postId: number | null;
  /** Ролик уже отправлялся: что вышло. Нужно подтверждение и повторный вызов с `force`. */
  alreadySent: string | null;
}
export interface Post {
  id: number;
  draftId: number;
  method: string;
  mode: string;
  status: "uploading" | "done" | "failed";
  detail: string;
  error: string | null;
  updatedAt: number;
}
export interface LoginEvent {
  state: "waiting" | "done" | "error";
  message: string;
}
export interface PostProgressEvent {
  draftId: number;
  fraction: number;
  message: string;
}
export interface PostDoneEvent {
  draftId: number;
  detail: string;
}
export interface PostFailedEvent {
  draftId: number;
  message: string;
}

export interface YoutubePreset {
  /** "720" | "1080" | "1440" | "2160" | "best" */
  key: string;
  label: string;
  /** У видео есть кадр не ниже. «Лучшее» доступно всегда. */
  available: boolean;
  /** Примерный размер файла; null — YouTube не сообщил. */
  approxBytes: number | null;
}
export interface YoutubeInfo {
  id: string;
  /** Каноничная ссылка watch?v=… */
  url: string;
  title: string;
  channel: string;
  duration: number | null;
  /** Адрес обложки на локальном медиа-сервере; null — обложку получить не удалось. */
  thumb: string | null;
  maxHeight: number;
  bestFps: number | null;
  bestCodec: string | null;
  hdr: boolean;
  presets: YoutubePreset[];
}
export interface YoutubeStart {
  url: string;
  preset: string;
  title: string;
  channel: string;
  thumb: string | null;
}
export type DownloadStage = "queued" | "downloading" | "merging" | "finishing" | "importing";
export interface StreamView {
  kind: "video" | "audio" | "combined";
  label: string;
  fraction: number | null;
  finished: boolean;
}
/** Полный снимок загрузки: интерфейс просто заменяет у себя запись. */
export interface YoutubeProgressEvent {
  jobId: string;
  videoId: string;
  url: string;
  preset: string;
  title: string;
  channel: string;
  thumb: string | null;
  stage: DownloadStage;
  downloaded: number;
  total: number | null;
  fraction: number | null;
  speed: number | null;
  eta: number | null;
  streams: StreamView[];
}
export interface YoutubeDoneEvent {
  jobId: string;
  project: Project;
}
export interface YoutubeErrorEvent {
  jobId: string;
  message: string;
  cancelled: boolean;
}

export const mockBus = new EventTarget();

export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const real = {
  appInfo: () => invoke<AppInfo>("app_info"),
  mediaUrl: (path: string) => invoke<string>("media_url", { path }),
  importVideo: (path: string) => invoke<Project>("import_video", { path }),
  listProjects: () => invoke<Project[]>("list_projects"),
  deleteProject: (id: number) => invoke<void>("delete_project", { id }),
  projectSetTight: (id: number, tight: boolean) => invoke<Project>("project_set_tight", { id, tight }),
  modelsStatus: () => invoke<ModelsStatus>("models_status"),
  analysisStart: (projectId: number, options?: AnalysisOptions) =>
    invoke<void>("analysis_start", { projectId, options }),
  analysisCancel: (projectId: number) => invoke<void>("analysis_cancel", { projectId }),
  analysisNotes: (projectId: number) => invoke<string[]>("analysis_notes", { projectId }),
  momentsList: (projectId: number) => invoke<Moment[]>("moments_list", { projectId }),
  momentSetStatus: (id: number, status: MomentStatus) => invoke<void>("moment_set_status", { id, status }),
  momentUpdate: (id: number, start: number, end: number, climax?: number) =>
    invoke<Moment>("moment_update", { id, start, end, climax }),
  momentAddManual: (projectId: number, start: number, end: number) =>
    invoke<Moment>("moment_add_manual", { projectId, start, end }),
  assetsList: () => invoke<Asset[]>("assets_list"),
  assetAdd: (kind: AssetKind, path: string) => invoke<Asset>("asset_add", { kind, path }),
  assetDelete: (id: number) => invoke<void>("asset_delete", { id }),
  draftsGenerate: (projectId: number) => invoke<number>("drafts_generate", { projectId }),
  draftsList: (projectId: number) => invoke<Draft[]>("drafts_list", { projectId }),
  draftSetStatus: (id: number, status: DraftStatus) => invoke<void>("draft_set_status", { id, status }),
  /** Одобряет все готовые черновики одним вызовом; возвращает, сколько одобрено. */
  draftsApproveReady: (projectId: number) => invoke<number>("drafts_approve_ready", { projectId }),
  draftUpdate: (id: number, config: DraftConfig, bounds?: { start: number; end: number; climax: number }) =>
    invoke<Moment>("draft_update", { id, config, bounds }),
  draftRerender: (id: number) => invoke<void>("draft_rerender", { id }),
  /** Переставляет фон по таймкодам у черновиков, которые ещё не одобрены; возвращает, сколько изменено. */
  draftsRealignGameplay: (projectId: number) => invoke<number>("drafts_realign_gameplay", { projectId }),
  gameplayTape: () => invoke<GameplayTape | null>("gameplay_tape"),
  gameplayTapeReset: () => invoke<void>("gameplay_tape_reset"),
  draftCancel: (id: number) => invoke<void>("draft_cancel", { id }),
  defaultsGet: () => invoke<Defaults>("defaults_get"),
  defaultsSet: (defaults: Defaults) => invoke<void>("defaults_set", { defaults }),
  finalsStart: (projectId: number) => invoke<number>("finals_start", { projectId }),
  outputDirGet: () => invoke<string>("output_dir_get"),
  outputDirSet: (path: string) => invoke<void>("output_dir_set", { path }),
  outputDirOpen: () => invoke<void>("output_dir_open"),
  finalOpen: (id: number, folder: boolean) => invoke<void>("final_open", { id, folder }),
  tiktokStatus: () => invoke<TikTokStatus>("tiktok_status"),
  tiktokSaveSettings: (args: TikTokSettings) => invoke<TikTokStatus>("tiktok_save_settings", { args }),
  tiktokForgetSecret: () => invoke<TikTokStatus>("tiktok_forget_secret"),
  tiktokLoginStart: () => invoke<void>("tiktok_login_start"),
  tiktokLoginCancel: () => invoke<void>("tiktok_login_cancel"),
  tiktokLogout: () => invoke<TikTokStatus>("tiktok_logout"),
  tiktokPublish: (args: PublishArgs) => invoke<PublishStart>("tiktok_publish", { args }),
  postsList: (projectId: number) => invoke<Post[]>("posts_list", { projectId }),
  youtubeAnalyze: (url: string) => invoke<YoutubeInfo>("youtube_analyze", { url }),
  youtubeDownload: (args: YoutubeStart) => invoke<string>("youtube_download", { args }),
  youtubeCancel: (jobId: string) => invoke<void>("youtube_cancel", { jobId }),
};

// Заглушка нужна только чтобы смотреть вёрстку в обычном браузере (`npm run dev`).
const demoProject: Project = {
  id: 1,
  path: "/demo/podcast.mp4",
  title: "podcast_ep42_full",
  duration: 5423,
  width: 1920,
  height: 1080,
  fps: 30,
  has_audio: true,
  status: "imported",
  created_at: 0,
  tight: false,
};
const demoMoments: Moment[] = [
  ["Он не ожидал такого поворота", "Вы не поверите, что случилось вчера. Возвращаюсь, а кот сидит на плите и смотрит на меня как ни в чём не бывало!", 0.91],
  ["Главная ошибка новичков", "Самое важное правило, которое все нарушают в первую неделю. Давайте разберём по шагам, почему так происходит.", 0.84],
  ["Тот самый момент в финале", "И тут зал взрывается аплодисментами. Никто не ожидал, что он решится на этот ход в последнюю минуту.", 0.77],
  ["Смешной фейл со стрима", "Ну всё, это конец. Нет, подожди, это не может быть правдой, я же только что это проверял!", 0.69],
].map(([title, transcript, score], i) => ({
  id: i + 1, projectId: 1, start: 120 + i * 340, end: 120 + i * 340 + 38 + i * 4, score: score as number,
  signalScore: (score as number) - 0.06, llmScore: (score as number) + 0.05, title: title as string,
  caption: "Смотрите до конца, там самое интересное", hashtags: ["#юмор", "#тикток", "#момент"],
  climax: 120 + i * 340 + 22, transcript: transcript as string, status: "candidate" as MomentStatus, manual: false,
}));

const demoAssets: Asset[] = [
  { id: 1, kind: "banner", path: "/demo/banner.mp4", name: "Промо-баннер", duration: 4, width: 640, height: 360, hasAudio: true, thumb: null },
  { id: 2, kind: "background", path: "/demo/parkour.mp4", name: "Minecraft паркур", duration: 900, width: 1080, height: 1920, hasAudio: false, thumb: null },
  { id: 3, kind: "background", path: "/demo/cs.mp4", name: "CS геймплей", duration: 600, width: 1920, height: 1080, hasAudio: false, thumb: null },
  { id: 4, kind: "music", path: "/demo/lofi.mp3", name: "Lo-fi бит", duration: 180, width: 0, height: 0, hasAudio: true, thumb: null },
];
// Проекты, добавленные в браузерной заглушке скачиванием с YouTube.
const extraProjects: Project[] = [];
const demoThumb =
  "data:image/svg+xml;utf8," +
  encodeURIComponent(
    "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 320 180'><defs><linearGradient id='g' x1='0' y1='0' x2='1' y2='1'><stop offset='0' stop-color='#3a2a7a'/><stop offset='1' stop-color='#0f6b6b'/></linearGradient></defs><rect width='320' height='180' fill='url(#g)'/><circle cx='160' cy='90' r='34' fill='white' fill-opacity='.16'/><path d='M150 72l30 18-30 18z' fill='white' fill-opacity='.85'/></svg>",
  );
const demoYoutube = (link: string): YoutubeInfo => {
  const low = /hd720/.test(link); // ссылка с «hd720» показывает видео, где выше 720p нет
  const mb = (n: number) => n * 1024 * 1024;
  return {
    id: "dQw4w9WgXcQ",
    url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
    title: "Как мы собрали ракету из подручных материалов (полная версия)",
    channel: "Технопарк",
    duration: 3722,
    thumb: demoThumb,
    maxHeight: low ? 720 : 2160,
    bestFps: low ? 30 : 60,
    bestCodec: low ? "H.264" : "VP9",
    hdr: false,
    presets: [
      { key: "720", label: "720p", available: true, approxBytes: mb(380) },
      { key: "1080", label: "1080p", available: !low, approxBytes: low ? null : mb(690) },
      { key: "1440", label: "1440p", available: !low, approxBytes: low ? null : mb(1800) },
      { key: "2160", label: "4K", available: !low, approxBytes: low ? null : mb(3900) },
      { key: "best", label: "Лучшее", available: true, approxBytes: low ? mb(380) : mb(3900) },
    ],
  };
};
const demoTimers = new Map<string, ReturnType<typeof setInterval>>();
let demoJobSeq = 0;
let demoDefaults: Defaults = { style: defaultStyle, bannerId: 1, backgroundIds: [2, 3], musicId: 4 };
let demoTape: GameplayTape | null = { assetId: 2, pos: 654 };
const demoDrafts: Draft[] = [];
// Для просмотра экрана «Финал» в браузере: пока черновики не собраны, показываем пару готовых роликов.
const demoFinals: Draft[] = [0, 1].map((i) => ({
  id: 200 + i, momentId: i + 1, projectId: 1, part: i + 1, title: ["Он не ожидал такого поворота", "Главная ошибка новичков"][i],
  caption: "Смотрите до конца, там самое интересное", hashtags: ["#юмор", "#тикток"], start: 120, end: 160, climax: 142,
  status: "final_done", proxyPath: null, posterPath: null, finalPath: `/demo/final_${i + 1}.mp4`, error: null, updatedAt: 1,
  config: { style: defaultStyle, bannerId: 1, backgroundId: 2, gameplayOffset: 0, musicId: 4, musicOffset: 0 },
}));
let demoTikTok: TikTokStatus = {
  clientKey: "", port: 38475, mode: "inbox", method: "api", privacy: "SELF_ONLY", autoPublish: false, hasSecret: false,
  redirectUri: "http://127.0.0.1:38475/callback/", connected: false, refreshExpiresAt: null, scope: "", secretsBackend: "keyring",
  chromium: true, loginInProgress: false,
};

const mock: typeof real = {
  appInfo: async () => ({
    version: "dev",
    data_dir: "",
    env: { os: "linux", ffmpeg: "ffmpeg", nvenc: true, libass: true, ytdlp: "2026.10.26", ytdlpOk: true, jsRuntime: "deno" },
  }),
  mediaUrl: async () => "",
  importVideo: async () => demoProject,
  listProjects: async () => [...extraProjects, demoProject],
  deleteProject: async () => {},
  projectSetTight: async (id, tight) => {
    const p = [demoProject, ...extraProjects].find((x) => x.id === id) ?? demoProject;
    p.tight = tight;
    return { ...p };
  },
  modelsStatus: async () => ({ dir: "~/.local/share/…/models", whisper: true, yamnet: true, llm: true, worker: true }),
  analysisStart: async (projectId) => {
    const stages: [string, string][] = [["audio", "Читаю звук"], ["events", "Ищу смех, крики и аплодисменты"], ["speech", "Распознаю речь"], ["video", "Считаю динамику кадра"], ["score", "Выбираю лучшие места"], ["llm", "Оцениваю моменты"]];
    let i = 0;
    const tick = () => {
      if (i >= stages.length * 8) {
        mockBus.dispatchEvent(new CustomEvent("analysis-done", { detail: { projectId, moments: demoMoments.length, notes: [] } }));
        return;
      }
      const [stage, message] = stages[Math.floor(i / 8)];
      mockBus.dispatchEvent(new CustomEvent("analysis-progress", { detail: { projectId, stage, fraction: i / (stages.length * 8), message } }));
      i++;
      setTimeout(tick, 220);
    };
    tick();
  },
  analysisCancel: async () => {},
  analysisNotes: async () => [],
  momentsList: async () => demoMoments,
  momentSetStatus: async (id, status) => void (demoMoments.find((m) => m.id === id)!.status = status),
  momentUpdate: async (id, start, end, climax) => {
    const m = demoMoments.find((x) => x.id === id)!;
    Object.assign(m, { start, end, climax: climax ?? m.climax });
    return m;
  },
  assetsList: async () => demoAssets,
  assetAdd: async (kind, path) => ({ ...demoAssets[0], id: 50 + demoAssets.length, kind, path, name: path.split(/[\\/]/).pop() ?? "файл" }),
  assetDelete: async () => {},
  draftsGenerate: async (projectId) => {
    demoMoments.filter((m) => m.status !== "rejected").slice(0, 4).forEach((m, i) => {
      if (demoDrafts.some((d) => d.momentId === m.id)) return;
      demoDrafts.push({
        id: 100 + i, momentId: m.id, projectId, part: i + 1, title: m.title, caption: m.caption, hashtags: m.hashtags,
        start: m.start, end: m.end, climax: m.climax, status: i === 2 ? "rendering" : "ready", proxyPath: null, posterPath: null,
        finalPath: null, error: null, updatedAt: 1,
        config: { style: defaultStyle, bannerId: 1, backgroundId: i % 2 ? 3 : 2, gameplayOffset: 40 * i, musicId: 4, musicOffset: 10 },
      });
    });
    return demoDrafts.length;
  },
  draftsList: async () => (demoDrafts.length ? demoDrafts : demoFinals),
  draftSetStatus: async (id, status) => void (demoDrafts.find((d) => d.id === id)!.status = status),
  draftsApproveReady: async () => {
    const ready = demoDrafts.filter((d) => d.status === "ready");
    ready.forEach((d) => (d.status = "approved"));
    return ready.length;
  },
  draftUpdate: async (id, config) => {
    const d = demoDrafts.find((x) => x.id === id)!;
    d.config = config;
    return demoMoments.find((m) => m.id === d.momentId)!;
  },
  draftRerender: async () => {},
  draftsRealignGameplay: async () => 0,
  gameplayTape: async () => demoTape,
  gameplayTapeReset: async () => void (demoTape = null),
  draftCancel: async () => {},
  defaultsGet: async () => demoDefaults,
  defaultsSet: async (d) => void (demoDefaults = d),
  finalsStart: async () => 0,
  outputDirGet: async () => "~/Videos/Автонарезка",
  outputDirSet: async () => {},
  outputDirOpen: async () => {},
  finalOpen: async () => {},
  tiktokStatus: async () => demoTikTok,
  tiktokSaveSettings: async (a) => (demoTikTok = { ...demoTikTok, ...a, hasSecret: demoTikTok.hasSecret || !!a.clientSecret, connected: a.clientKey === demoTikTok.clientKey && demoTikTok.connected }),
  tiktokForgetSecret: async () => (demoTikTok = { ...demoTikTok, hasSecret: false, connected: false }),
  tiktokLoginStart: async () => {
    mockBus.dispatchEvent(new CustomEvent("tiktok-login", { detail: { state: "waiting", message: "Подтверди доступ в открывшемся браузере" } }));
    setTimeout(() => {
      demoTikTok = { ...demoTikTok, connected: true, refreshExpiresAt: Math.floor(Date.now() / 1000) + 86400 * 180 };
      mockBus.dispatchEvent(new CustomEvent("tiktok-login", { detail: { state: "done", message: "Аккаунт подключён" } }));
    }, 1500);
  },
  tiktokLoginCancel: async () => {},
  tiktokLogout: async () => (demoTikTok = { ...demoTikTok, connected: false }),
  tiktokPublish: async (a) => {
    let f = 0;
    const tick = () => {
      f += 0.1;
      if (f >= 1) {
        mockBus.dispatchEvent(new CustomEvent("post-done", { detail: { draftId: a.draftId, detail: "Ролик в черновиках TikTok. Открой приложение TikTok, вставь подпись и опубликуй." } }));
        return;
      }
      mockBus.dispatchEvent(new CustomEvent("post-progress", { detail: { draftId: a.draftId, fraction: f, message: `Загружаю в TikTok: ${Math.round(f * 40)} из 40 МБ` } }));
      setTimeout(tick, 250);
    };
    tick();
    return { postId: 1, alreadySent: null };
  },
  postsList: async () => [],
  youtubeAnalyze: async (text) => {
    await new Promise((r) => setTimeout(r, 700));
    const link = extractYoutubeLink(text) ?? text.trim();
    if (!link) throw "Вставьте ссылку на YouTube-видео.";
    if (/playlist\?list=/.test(link)) throw "Плейлисты пока не поддерживаются — вставьте ссылку на одно видео.";
    if (!/(youtube\.com|youtu\.be)\//i.test(link)) throw "Не YouTube-ссылка.";
    if (/aaaaaaaaaaa/.test(link)) throw "[youtube] aaaaaaaaaaa: Video unavailable";
    return demoYoutube(link);
  },
  youtubeDownload: async (a) => {
    const jobId = `yt${++demoJobSeq}`;
    const base = { jobId, videoId: "dQw4w9WgXcQ", url: a.url, preset: a.preset, title: a.title, channel: a.channel, thumb: a.thumb };
    const emit = (p: Omit<YoutubeProgressEvent, keyof typeof base>) =>
      mockBus.dispatchEvent(new CustomEvent("youtube-progress", { detail: { ...base, ...p } }));
    const idle = { downloaded: 0, total: null, fraction: null, speed: null, eta: null, streams: [] };
    emit({ ...idle, stage: "queued" });

    const failing = a.url.includes("bbbbbbbbbbb");
    const V = 620 * 1024 * 1024;
    const A = 48 * 1024 * 1024;
    const VIDEO_STEPS = 32;
    const AUDIO_STEPS = 5;
    let step = 0;
    const timer = setInterval(() => {
      step++;
      if (failing && step > 6) {
        clearInterval(timer);
        demoTimers.delete(jobId);
        mockBus.dispatchEvent(new CustomEvent("youtube-error", { detail: { jobId, message: "unable to download video data: HTTP Error 403: Forbidden", cancelled: false } }));
      } else if (step <= VIDEO_STEPS) {
        const f = step / VIDEO_STEPS;
        // как в настоящем подсчёте: пока идёт видео, общий размер — только его размер
        emit({ stage: "downloading", downloaded: V * f, total: V, fraction: f, speed: 9.2 * 1024 * 1024, eta: Math.round((VIDEO_STEPS - step) * 0.25), streams: [{ kind: "video", label: "1080p · H.264", fraction: f, finished: f >= 1 }] });
      } else if (step <= VIDEO_STEPS + AUDIO_STEPS) {
        const f = (step - VIDEO_STEPS) / AUDIO_STEPS;
        emit({ stage: "downloading", downloaded: V + A * f, total: V + A, fraction: 1, speed: 6.1 * 1024 * 1024, eta: Math.round((AUDIO_STEPS - (step - VIDEO_STEPS)) * 0.25), streams: [{ kind: "video", label: "1080p · H.264", fraction: 1, finished: true }, { kind: "audio", label: "130 kbps · AAC", fraction: f, finished: f >= 1 }] });
      } else if (step === VIDEO_STEPS + AUDIO_STEPS + 1) {
        emit({ stage: "merging", downloaded: V + A, total: V + A, fraction: 1, speed: null, eta: null, streams: [] });
      } else if (step === VIDEO_STEPS + AUDIO_STEPS + 5) {
        emit({ stage: "finishing", downloaded: V + A, total: V + A, fraction: 1, speed: null, eta: null, streams: [] });
      } else if (step === VIDEO_STEPS + AUDIO_STEPS + 7) {
        emit({ stage: "importing", downloaded: V + A, total: V + A, fraction: 1, speed: null, eta: null, streams: [] });
      } else if (step > VIDEO_STEPS + AUDIO_STEPS + 8) {
        clearInterval(timer);
        demoTimers.delete(jobId);
        const project: Project = { ...demoProject, id: 100 + demoJobSeq, title: a.title, path: `/demo/${a.title}.mp4`, duration: 3722, created_at: Date.now() };
        extraProjects.unshift(project);
        mockBus.dispatchEvent(new CustomEvent("youtube-done", { detail: { jobId, project } }));
      }
    }, 250);
    demoTimers.set(jobId, timer);
    return jobId;
  },
  youtubeCancel: async (jobId) => {
    const timer = demoTimers.get(jobId);
    if (timer === undefined) return;
    clearInterval(timer);
    demoTimers.delete(jobId);
    mockBus.dispatchEvent(new CustomEvent("youtube-error", { detail: { jobId, message: "Остановлено — можно продолжить", cancelled: true } }));
  },
  momentAddManual: async (_p, start, end) => ({ ...demoMoments[0], id: 99, start, end, title: "Мой момент", status: "accepted", manual: true }),
};

export const api = inTauri || !import.meta.env.DEV ? real : mock;

export function fmtDuration(sec: number): string {
  const s = Math.max(0, Math.round(sec));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  const mm = h > 0 ? String(m).padStart(2, "0") : String(m);
  return `${h > 0 ? `${h}:` : ""}${mm}:${String(r).padStart(2, "0")}`;
}

export function errText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e);
}
