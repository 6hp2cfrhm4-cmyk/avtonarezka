import { invoke } from "@tauri-apps/api/core";
import { captionLen, normalizeTags } from "./caption";
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
  twitchDownloader: string | null;
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
  /** Все модели на месте. */
  ready: boolean;
  /** Сейчас качаются. */
  downloading: boolean;
}
export interface ModelsProgressEvent {
  /** Путь файла внутри папки моделей, например `qwen3-4b-q4_k_m/Qwen3-4B-Q4_K_M.gguf`. */
  name: string;
  received: number;
  total: number;
}
export interface ModelsErrorEvent {
  message: string;
}

export interface AnalysisOptions {
  useEvents?: boolean;
  useSpeech?: boolean;
  useLlm?: boolean;
  minLen?: number;
  maxLen?: number;
  topK?: number;
  gpuLayers?: number;
  language?: string;
  fixedSeconds?: number | null;
  weights?: { loudness: number; motion: number; events: number; speech: number; chat: number; viewer_clips: number; camera: number };
  llmTop?: number;
  llmWeight?: number;
  scoutEnabled?: boolean;
  scoutCoverage?: number;
}

export type AnalysisSettings = Required<AnalysisOptions>;
export const defaultAnalysisSettings: AnalysisSettings = {
  useEvents: true, useSpeech: true, useLlm: true, minLen: 20, maxLen: 60, topK: 30,
  gpuLayers: 99, language: "ru", fixedSeconds: null,
  weights: { loudness: 0.35, motion: 0.15, events: 0.35, speech: 0.15, chat: 0.15, viewer_clips: 0.2, camera: 0.15 }, scoutEnabled: true, scoutCoverage: 0.25, llmTop: 16, llmWeight: 0.5,
};

export interface ScoutReport { windows: { start: number; end: number }[]; duration: number; selectedSeconds: number; speechSeconds: number; full: boolean }
export interface AnalysisTimeline { duration: number; hop: number; series: { key: string; label: string; values: number[] | null }[]; scout: ScoutReport | null }
export interface AnalysisInputs { chat: { format: string; messages: number; ignored: number } | null; clips: { videoId: string; clips: number; missingOffset: number; truncated: boolean } | null; chatError: string | null }
export interface TwitchSignalStatus { configured: boolean; storage: string }
function normalizeAnalysis(settings: Partial<AnalysisSettings>): AnalysisSettings {
  return { ...defaultAnalysisSettings, ...settings, weights: { ...defaultAnalysisSettings.weights, ...settings.weights } };
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
  alpha?: boolean;
  greenCorners?: number | null;
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
  alpha?: boolean;
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
  layout: "gameplayStack" | "reactionPip" | "reactionStack";
  reaction: { background: boolean; circle: boolean; sizePct: number; position: "seamLeft" | "seamCenter" | "bottomRight"; zoom: number; ringPx: number; ringColor: string; shadow: boolean; pop: boolean; freeze: boolean };
  split: number;
  topFit: TopFit;
  /** Доля высоты исходника, срезаемая снизу (вшитые в видео субтитры). */
  topCropBottom: number;
  gameplayFreeze: boolean;
  banner: BannerStyle;
  subtitles: SubtitleStyle;
  partLabel: PartLabelStyle;
  hookEnabled: boolean;
  audio: AudioMix;
}
export interface DraftConfig {
  camRect?: CameraRect | null;
  faceCheck?: { frames: number; found: number; longestGap: number; median: CameraRect | null; error: string | null } | null;
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
  layout: "gameplayStack",
  reaction: { background: true, circle: true, sizePct: 0.30, position: "seamLeft", zoom: 1.1, ringPx: 6, ringColor: "ffffff", shadow: true, pop: true, freeze: true },
  split: 0.5,
  topFit: "auto",
  topCropBottom: 0,
  gameplayFreeze: false,
  banner: { enabled: true, keyColor: "00ff00", similarity: 0.18, blend: 0.08, despill: true, widthPct: 0.72, offsetX: 0, offsetY: 0, anim: "pop", maxSeconds: null },
  subtitles: { enabled: true, font: "Montserrat ExtraBold", size: 84, color: "ffffff", highlight: "b6ff3c", outlineColor: "000000", outline: 7, uppercase: true, wordsPerLine: 3, highlightScale: 112, yPct: null },
  hookEnabled: false,
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

/** `drafting` — пишется описание, `ready` — готово и ждёт запуска, `needs_check` — неясно, вышел ли ролик: проверь в TikTok. */
export type JobState = "drafting" | "ready" | "queued" | "sending" | "done" | "failed" | "needs_check";
/** Готовый ролик из папки: уже отрендеренный, выкладывается без рендера. */
export interface ReadyVideo {
  id: number;
  name: string;
  duration: number;
  width: number;
  height: number;
  /** Не вертикальный кадр: ролик добавлен, но TikTok его обрежет или покажет с полями. */
  warning: string | null;
}
export interface PublishJob {
  id: number;
  /** Ролик из пачки. У готового ролика из папки его нет. */
  draftId: number | null;
  /** Готовый ролик из папки. */
  readyId: number | null;
  /** Имя файла у готового ролика; у ролика из пачки пусто. */
  name: string;
  /** Аккаунт, выбранный вручную; `null` — по кругу. */
  wantedAccount: number | null;
  caption: string;
  hashtags: string[];
  state: JobState;
  error: string | null;
  attempts: number;
  /** Не раньше этого времени (unix, секунды); 0 — можно сразу. */
  notBefore: number;
  /** Длина подписи вместе с хэштегами, как её соберёт бэкенд. */
  len: number;
  updatedAt: number;
}
export interface QueueSettings {
  pauseMin: number;
  /** Не больше роликов в местные сутки на каждый аккаунт. */
  dailyLimit: number;
  /** Окно публикации: минуты от местной полуночи. Равные начало и конец — без ограничения; начало больше конца — через полночь. */
  windowStart: number;
  windowEnd: number;
}
export interface AccountInfo {
  id: number;
  label: string;
  enabled: boolean;
  /** Основной аккаунт: им выкладываются ручные публикации и всё, что ушло до пула. */
  main: boolean;
  /** Выложено сегодня (по местным суткам). */
  sentToday: number;
}
export interface QueueState {
  jobs: PublishJob[];
  settings: QueueSettings;
  /** Пул аккаунтов: круг, по которому идут ролики. */
  accounts: AccountInfo[];
  /** Два и больше включённых аккаунта: ролики выходят без надписи «Часть N». */
  pool: boolean;
  /** Очередь идёт (цикл запущен). */
  running: boolean;
  /** Модель пишет описания. */
  generating: boolean;
  /** Выложено сегодня по всем аккаунтам (местные сутки). */
  sentToday: number;
  /** Когда уйдёт следующий ролик (unix), если очередь ждёт. */
  nextAt: number | null;
  /** Почему очередь стоит или ждёт. */
  note: string | null;
  /** Окно длины подписи и предел хэштегов: решает бэкенд, интерфейс только показывает. */
  windowMin: number;
  windowMax: number;
  maxTags: number;
  /** Сколько знаков принимает страница TikTok (вместе с хэштегами): потолок для любого текста, своего или от модели. */
  captionLimit: number;
  chromium: boolean;
}
export type JobResolve = "retry" | "mark_done" | "remove";
export interface QueueTextEvent {
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
/** Пачка: элемент и его стадия. `awaiting_approval` — черновики ждут человека, `rendering` — идёт финал. */
export type BatchStage = "discovered" | "fetching" | "scouting" | "selecting" | "awaiting_selection" | "qc" | "auto_approved" | "rendering_final" | "captioning" | "ready_to_schedule" | "scheduled" | "publishing" | "published" | "measured" | "completed" | "needs_check" | "queued" | "downloading" | "importing" | "analyzing" | "drafting" | "awaiting_approval" | "rendering" | "done" | "failed";
export interface BatchItem {
  id: number;
  kind: "link" | "file";
  source: string;
  title: string;
  precut: boolean;
  stage: BatchStage;
  /** Для «ошибки» — причина, для «готово» — заметка, сколько черновиков не собрались. */
  error: string | null;
  projectId: number | null;
  channelId: number | null;
  priority: number;
  position: number;
  paused: boolean;
}
export interface BatchNewItem {
  channelId?: number | null;
  priority?: number;
  kind: "link" | "file";
  source: string;
  title?: string;
  preset?: string;
  precut?: boolean;
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

export interface FactoryTemplate { id: number; name: string; layout: "gameplay_stack" | "reaction_pip" | "reaction_stack"; config: Defaults; isDefault: boolean; previewUrl?: string }
export interface ChannelTarget { platform: "tiktok" | "youtube" | "instagram" | "vk" | "telegram"; accountId: number | null }
export interface MonitorOptions {
  enabled: boolean; intervalMinutes: number; minDuration: number | null; maxDuration: number | null;
  titleRegex: string; cookiesBrowser: string | null; twoPass: boolean; downloadChat: boolean; viewerClips: boolean;
}
export const defaultMonitorOptions: MonitorOptions = { enabled: false, intervalMinutes: 15, minDuration: null, maxDuration: null, titleRegex: "", cookiesBrowser: null, twoPass: true, downloadChat: false, viewerClips: false };
export interface SourcePollResult { found: number; queued: number; skipped: number; seen: number }
export interface CameraRect { x: number; y: number; width: number; height: number }
export interface CameraFrame { url: string; width: number; height: number; duration: number; at: number }
export interface CameraScanProgress { id: string; frames: number; total: number; stage: "waiting" | "scanning" | "done" }
export interface CameraScanResult { check: { frames: number; found: number; longestGap: number; median: CameraRect | null; error: string | null }; suggested: CameraRect | null }
export interface PlatformCaps { platform: ChannelTarget["platform"]; name: string; captionMax: number; titleMax: number; hashtagsMax: number; durationMax: number | null; fileMax: number | null; dailyDefault: number; nativeSchedule: boolean; login: string }
export interface CaptionFields { title: string; body: string; credit: string; link: string; promo: string; hashtags: string[] }
export interface SavedCaption { adLocked?: boolean; spec: { platform: ChannelTarget["platform"]; template: string }; fields: CaptionFields }
export interface ComposedCaption { text: string; len: number; max: number; min: number; bodyTrimmed: boolean }
export interface OutputProfile { platform: ChannelTarget["platform"]; limits: { maxSeconds: number | null; videoKbps: number; audioKbps: number } }
export interface OutputVariant { url: string; posterUrl: string; duration: number; bytes: number; videoKbps: number; partRemoved: boolean; truncated: boolean; maxSeconds: number | null }
export interface ChannelIncident { id: number; channelId: number; publicationId: number | null; kind: "complaint" | "restriction"; note: string; createdAt: number; resolvedAt: number | null }
export interface Channel {
  id: number; name: string; sourceKind: "file" | "youtube" | "folder" | "twitch" | "kick";
  source: string; templateId: number | null; mode: "auto" | "review" | "manual"; enabled: boolean;
  analysis: AnalysisSettings; scoreThreshold: number; maxClips: number;
  complaintLimit: number; rightsPaused: boolean; activeIncidents: number;
  permission: "owned" | "agreed" | "unspecified"; credit: string; targets: ChannelTarget[];
  campaignIds: number[]; camRect: CameraRect | null;
  monitor: MonitorOptions; lastCheckedAt: number | null; nextCheckAt: number | null; sourceError: string | null;
}
export interface FactorySettings { draftConcurrency: number; finalConcurrency: number; resumeOnStart: boolean; inhibitSleep: boolean }
export interface PowerStatus { active: boolean; needed: boolean; reason: string; error: string | null }
export interface LifecycleStatus { trayEnabled: boolean; trayActive: boolean; trayError: string | null; autostartAvailable: boolean; autostartEnabled: boolean; autostartError: string | null; windowVisible: boolean }
export interface DiskPolicy { automatic: boolean; keepDays: number; maxGib: number }
export interface DiskSnapshot { policy: DiskPolicy; files: { path: string; channel: string; kind: string; bytes: number; createdAt: number; reason: string | null; selected: boolean }[]; channels: { id: number; name: string; bytes: number; protectedBytes: number; reclaimableBytes: number }[]; categories: { kind: string; bytes: number }[]; managedBytes: number; reclaimableBytes: number; selectedBytes: number; missing: number; limitUnreachable: boolean }
export interface DiskCleanup { deleted: number; bytes: number; errors: string[] }
export interface QcReport { version: number; path: string; fingerprint: string; issues: string[]; duration: number; checkedAt: number }
export interface FactoryEvent { id: number; itemId: number | null; kind: string; stage: string | null; message: string; createdAt: number }
export interface InboxItem { itemId: number; channelName: string; draft: Draft; qc: QcReport | null }
export interface CalendarEntry { draftId: number; projectId: number; title: string; channelName: string; state: string; notBefore: number | null; finalPath: string | null }
export interface PublicationPolicy { pauseMin: number; dailyLimit: number; windowStart: number; windowEnd: number }
export interface YoutubeSettings { privacy: "private" | "unlisted" | "public"; madeForKids: boolean }
export interface YoutubeOAuthSettings { clientId: string; hasSecret: boolean; secretBackend: string; loginRunning: boolean }
export interface PublicationAccount { id: number; label: string; platform: ChannelTarget['platform']; externalRef: string; enabled: boolean; policy: PublicationPolicy; hasSecret: boolean; youtubeSettings?: YoutubeSettings | null }
export interface InstagramAccountArgs { id: number | null; label: string; user: string; token: string | null; enabled: boolean; policy: PublicationPolicy }
export interface VkMetricsAccountArgs { id: number | null; label: string; video: string; token: string | null; enabled: boolean }
export interface VkMetricsClip { draftId: number | null; readyId: number | null; title: string; project: string }
export interface VkMetricsOptions { accounts: { id: number; label: string; owner: string; enabled: boolean; hasSecret: boolean; probeVideo: string }[]; clips: VkMetricsClip[]; secretBackend: string }
export interface VkMetricsLinkArgs { accountId: number; draftId: number | null; readyId: number | null; video: string; sentAt: number }
export interface TelegramAccountArgs { id: number | null; label: string; chat: string; token: string | null; enabled: boolean; policy: PublicationPolicy }
export interface TelegramConnectionCheck { botUsername: string; chatId: string; chatTitle: string; checkedAt: number }
export interface Publication { id: number; draftId: number | null; readyId: number | null; projectId: number; title: string; platform: ChannelTarget['platform']; accountId: number | null; accountLabel: string; state: string; notBefore: number; caption: string; externalId: string | null; url: string | null; error: string | null; legacy: boolean; holdReason: string | null; holdKind: "channel" | "final" | null }
export interface PublicationState { publications: Publication[]; running: boolean; note: string | null; secretBackend: string }
export interface FactoryState { items: BatchItem[]; events: FactoryEvent[]; inbox: InboxItem[]; paused: boolean; running: boolean }

export interface Campaign { id: number; name: string; advertiser: string; enabled: boolean; bannerIds: number[]; link: string; promo: string; utm: string; startsAt: number | null; endsAt: number | null; weight: number; dailyLimit: number | null; totalLimit: number | null; channelIds: number[]; platforms: string[] }
export interface CampaignPick { campaignId: number; bannerId: number }
export interface CampaignAssignment { draftId: number; campaignId: number; campaignName: string; advertiser: string; bannerId: number; assignedAt: number }
export interface MetricCounts { views: number | null; likes: number | null; comments: number | null; shares: number | null }
export interface MetricSample extends MetricCounts { publicationId: number; observedAt: number; source: "api" | "manual" }
export interface AnalyticsRow { id: number; draftId: number | null; title: string; platform: string; accountId: number | null; account: string; channelId: number | null; channel: string; state: string; sentAt: number | null; url: string | null; sample: MetricSample | null; error: string | null }
export interface AnalyticsDashboard { rows: AnalyticsRow[]; published: number; measured: number; views: number | null; medianViews: number | null; errors: number; attention: number; errorRate: number | null; hours: { hour: number; published: number; medianViews: number | null }[]; autoCollect: boolean; checkedAt: number | null }
export interface AnalyticsFeedback { channelId: number; clips: number; reason: string; suggestions: { signal: string; current: number; proposed: number; correlation: number; sampleSize: number }[] }

export const mockBus = new EventTarget();

export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export interface ServerConfig { host: string; port: number; user: string; keyPath: string; enabled: boolean }
export interface NodeFuse { failures: number; blocked: boolean; message: string | null }
export interface NodeSystem { load: number[]; mem_available_mb: number | null; psi: { cpu: number | null; memory: number | null; io: number | null }; disk_free_mb: number | null; data_bytes: number }
export interface NodeRemote { db: boolean; schema?: number; stages: Record<string, number>; system?: NodeSystem }
export interface NodeStatus { configured: boolean; enabled: boolean; config: ServerConfig | null; fuse: NodeFuse; online: boolean; error: string | null; remote: NodeRemote | null }
export interface NodeCandidate { id: number; title: string; source: string; stage: string; canMove: boolean; reason: string | null; ownerNode: string }
export interface NodeSendReport { sent: number[]; failed: [number, string][] }
export interface NodeHandover { sent: number[]; failed: [number, string][]; safeToPowerOff: boolean; staysHere: number[] }
export interface NodePulled { finished: number[]; failed: number[]; waiting: number }
export interface NodeHostKey { fingerprint: string; line: string }

export type FlowId = "live" | "studio" | "discovery" | "monitor" | "publish" | "offload";
export type Placement = "pc" | "server" | "auto";
export interface FlowSetting { flow: FlowId; enabled: boolean; placement: Placement }
export interface SceneDto { id: number; name: string; flows: FlowSetting[]; builtin: boolean }
export type RuleCond = { kind: "pc_online"; value: boolean } | { kind: "hours"; from: number; to: number } | { kind: "server_busy" } | { kind: "disk_low" } | { kind: "streamer_live" };
export interface RuleDto { id: number; name: string; cond: RuleCond; sceneId: number; enabled: boolean }
export interface FlowAction { flow: FlowId; node: "pc" | "server" | null; run: boolean; limit: number | null; reason: string }
export interface ModesState { node: "pc" | "server"; scenes: SceneDto[]; rules: RuleDto[]; active: number | null; pinnedUntil: number; actions: FlowAction[]; log: [number, string][] }

export interface TrendConfig { twitch: boolean; kick: boolean; youtube: boolean; topics: boolean; languages: string[]; categories: string[]; keywords: string[]; knownChannels: string[]; whitelist: string[]; threshold: number; showFrom: number; dailyLimit: number; intervalMin: number; region: string; gamesTop: number }
export interface TrendCandidate { id: number; kind: "live" | "vod" | "clip"; platform: string; url: string; title: string; category: string; language: string; channelName: string; metric: number; previous: number; windowMin: number; score: number; why: string; state: string; auto: boolean; lastSeen: number }
export interface TrendProvider { configured: boolean; ok: boolean | null; at: number; found: number; message: string }
export interface TrendState { config: TrendConfig; new: TrendCandidate[]; watch: TrendCandidate[]; accepted: TrendCandidate[]; providers: Record<string, TrendProvider>; topics: string[]; lastPoll: number; flowEnabled: boolean; youtubeKey: boolean; kickToken: boolean; twitchKeys: boolean }
export interface TrendStats { seen: number; fresh: number; shown: number; autoAccepted: number }

export interface ServerKey { keyPath: string; publicKey: string; version: string; created: boolean }
export interface PushReport { applied: string[]; skipped: string[] }
export interface RemoteConfig { secrets: Record<string, boolean>; kv: Record<string, string>; keyStore: boolean }

export interface NotifyConfig { enabled: boolean; chat: string; summaryHour: number; attention: boolean }
export interface NotifyState { config: NotifyConfig; token: boolean }

export interface LiveSession { id: number; platform: string; url: string; channel: string; title: string; state: "starting" | "live" | "ended" | "stopped" | "failed"; manual: boolean; chat: boolean; startedAt: number; endedAt: number | null; note: string; moments: number }
export interface LiveMoment { id: number; sessionId: number; score: number; manual: boolean; state: "cut" | "failed" | "ignored"; itemId: number | null; error: string | null; createdAt: number; length: number }
export interface LiveState { sessions: LiveSession[]; moments: LiveMoment[]; flowEnabled: boolean; maxSessions: number; diskFreeMb: number | null }
export interface LiveSignal { sessionId: number; t: number; z: number; msgs: number; authors: number; audioDb: number }

const real = {
  nodeKeygen: () => invoke<ServerKey>("node_keygen"),
  integrationTest: (name: "twitch" | "youtube" | "kick") => invoke<string>("integration_test", { name }),
  nodeConfigPush: (groups: string[]) => invoke<PushReport>("node_config_push", { groups }),
  nodeConfigRemote: () => invoke<RemoteConfig>("node_config_remote"),
  notifyState: () => invoke<NotifyState>("notify_state"),
  notifySet: (config: NotifyConfig, token?: string) => invoke<void>("notify_set", { config, token }),
  notifyTest: () => invoke<void>("notify_test"),
  liveState: () => invoke<LiveState>("live_state"),
  liveStart: (url: string) => invoke<number>("live_start", { url }),
  liveStop: (id: number) => invoke<void>("live_stop", { id }),
  liveCutNow: (id: number, seconds?: number) => invoke<void>("live_cut_now", { id, seconds }),
  liveMomentIgnore: (id: number) => invoke<void>("live_moment_ignore", { id }),
  discoveryState: () => invoke<TrendState>("discovery_state"),
  discoveryConfigSet: (config: TrendConfig) => invoke<TrendConfig>("discovery_config_set", { config }),
  discoverySecretSet: (name: "youtube" | "kick", value: string) => invoke<void>("discovery_secret_set", { name, value }),
  discoveryPollNow: () => invoke<TrendStats>("discovery_poll_now"),
  discoveryAccept: (id: number) => invoke<number | null>("discovery_accept", { id }),
  discoveryReject: (id: number) => invoke<void>("discovery_reject", { id }),
  notificationsEnabled: () => invoke<boolean>("notifications_enabled"),
  notificationsSet: (enabled: boolean) => invoke<void>("notifications_set", { enabled }),
  analyticsDashboard: (days: number, channelId: number | null) => invoke<AnalyticsDashboard>("analytics_dashboard", { days, channelId }),
  analyticsHistory: (publicationId: number) => invoke<MetricSample[]>("analytics_history", { publicationId }),
  analyticsManual: (publicationId: number, observedAt: number | null, counts: MetricCounts) => invoke<void>("analytics_manual", { publicationId, observedAt, counts }),
  analyticsCollect: () => invoke<{ collected: number; skipped: number; errors: string[] }>("analytics_collect"),
  vkMetricsOptions: () => invoke<VkMetricsOptions>("vk_metrics_options"),
  vkMetricsAccountSave: (args: VkMetricsAccountArgs) => invoke<number>("vk_metrics_account_save", { args }),
  vkMetricsAccountEnabled: (id: number, enabled: boolean) => invoke<void>("vk_metrics_account_enabled", { id, enabled }),
  vkMetricsLink: (args: VkMetricsLinkArgs) => invoke<number>("vk_metrics_link", { args }),
  analyticsAuto: (enabled: boolean) => invoke<void>("analytics_auto", { enabled }),
  analyticsFeedback: (channelId: number) => invoke<AnalyticsFeedback>("analytics_feedback", { channelId }),
  analyticsApply: (channelId: number, signal: string, current: number) => invoke<void>("analytics_apply", { channelId, signal, current }),
  campaignReportOpen: (campaignId: number) => invoke<string>("campaign_report_open", { campaignId }),
  campaignReportExport: (campaignId: number, path: string) => invoke<{ html: string; csv: string }>("campaign_report_export", { campaignId, path }),
  campaignsList: () => invoke<Campaign[]>("campaigns_list"),
  campaignSave: (campaign: Campaign) => invoke<Campaign>("campaign_save", { campaign }),
  campaignPreview: (channelId: number) => invoke<(CampaignPick | null)[]>("campaign_preview", { channelId }),
  campaignAssignments: () => invoke<CampaignAssignment[]>("campaign_assignments"),
  publicationAccounts: () => invoke<PublicationAccount[]>("publication_accounts"),
  publicationAccountPolicy: (id: number, policy: PublicationPolicy) => invoke<void>("publication_account_policy", { id, policy }),
  instagramAccountSave: (args: InstagramAccountArgs) => invoke<number>("instagram_account_save", { args }),
  telegramAccountSave: (args: TelegramAccountArgs) => invoke<number>("telegram_account_save", { args }),
  telegramAccountCheck: (args: { id: number | null; chat: string; token: string | null }) => invoke<TelegramConnectionCheck>("telegram_account_check", { args }),
  youtubeOAuthSettings: () => invoke<YoutubeOAuthSettings>("youtube_oauth_settings"),
  youtubeOAuthSave: (clientId: string, clientSecret: string | null) => invoke<void>("youtube_oauth_save", { clientId, clientSecret }),
  youtubeLoginStart: () => invoke<string>("youtube_login_start"),
  youtubeLoginCancel: () => invoke<void>("youtube_login_cancel"),
  youtubeAccountSave: (id: number, label: string, enabled: boolean, policy: PublicationPolicy, settings: YoutubeSettings) => invoke<void>("youtube_account_save", { id, label, enabled, policy, settings }),
  publicationsList: () => invoke<PublicationState>("publications_list"),
  publicationAutofill: () => invoke<{ scheduled: number; existing: number; blocked: string[] }>("publication_autofill"),
  publicationSchedule: (draftId: number, accountId: number, at: number | null) => invoke<number>("publication_schedule", { draftId, accountId, at }),
  publicationMove: (id: number, at: number) => invoke<void>("publication_move", { id, at }),
  publicationRefreshCaption: (id: number) => invoke<void>("publication_refresh_caption", { id }),
  publicationCancel: (id: number) => invoke<void>("publication_cancel", { id }),
  publicationResolve: (id: number, action: "mark_done" | "retry") => invoke<void>("publication_resolve", { id, action }),
  publicationsStart: () => invoke<void>("publications_start"),
  factoryCalendar: () => invoke<CalendarEntry[]>("factory_calendar"),
  platformCaps: () => invoke<PlatformCaps[]>("platform_caps"),
  outputProfiles: () => invoke<OutputProfile[]>("output_profiles"),
  outputProfileSave: (profile: OutputProfile) => invoke<OutputProfile>("output_profile_save", { profile }),
  outputPreview: (draftId: number, platform: ChannelTarget["platform"]) => invoke<OutputVariant>("output_preview", { draftId, platform }),
  captionDraftLoad: (draftId: number, platform: ChannelTarget["platform"]) => invoke<SavedCaption>("caption_draft_load", { draftId, platform }),
  captionDraftSave: (draftId: number, saved: SavedCaption) => invoke<ComposedCaption>("caption_draft_save", { draftId, saved }),
  captionAccountLoad: (draftId: number, platform: ChannelTarget["platform"], accountId: number | null) => invoke<SavedCaption>("caption_account_load", { draftId, platform, accountId }),
  captionAccountSave: (draftId: number, saved: SavedCaption, accountId: number | null) => invoke<ComposedCaption>("caption_account_save", { draftId, saved, accountId }),
  captionApplyBody: (draftIds: number[], platform: ChannelTarget["platform"], accountId: number | null, body: string) => invoke<number>("caption_apply_body", { draftIds, platform, accountId, body }),
  captionDraftGenerate: (draftId: number, saved: SavedCaption, accountId: number | null) => invoke<{ saved: SavedCaption; composed: ComposedCaption }>("caption_draft_generate", { draftId, saved, accountId }),
  accountsList: () => invoke<AccountInfo[]>("accounts_list"),
  channelsList: () => invoke<Channel[]>("channels_list"),
  cameraFrame: (projectId: number, at: number) => invoke<CameraFrame>("camera_frame", { projectId, at }),
  cameraDetect: (projectId: number, at: number) => invoke<{ rect: CameraRect; confidence: number }[]>("camera_detect", { projectId, at }),
  cameraScan: (projectId: number, id: string, camera: CameraRect) => invoke<CameraScanResult>("camera_scan", { projectId, id, camera }),
  cameraScanCancel: (id: string) => invoke<void>("camera_scan_cancel", { id }),
  layoutPreview: (channelId: number, projectId: number, at: number, template: FactoryTemplate) => invoke<{ url: string; facePresent: boolean; note: string }>("layout_preview", { channelId, projectId, at, template }),
  faceModelReady: () => invoke<boolean>("face_model_ready"),
  faceModelDownload: () => invoke<boolean>("face_model_download"),
  sourceCheck: (id: number) => invoke<SourcePollResult>("source_check", { id }),
  channelIncidents: (channelId: number) => invoke<ChannelIncident[]>("channel_incidents", { channelId }),
  channelIncidentAdd: (args: { channelId: number; publicationId: number | null; kind: ChannelIncident["kind"]; note: string }) => invoke<number>("channel_incident_add", { args }),
  channelIncidentResolve: (id: number) => invoke<void>("channel_incident_resolve", { id }),
  channelRightsResume: (channelId: number) => invoke<void>("channel_rights_resume", { channelId }),
  channelSave: (channel: Channel) => invoke<Channel>("channel_save", { channel }),
  channelRemove: (id: number) => invoke<void>("channel_remove", { id }),
  templatesList: () => invoke<FactoryTemplate[]>("templates_list"),
  templateSave: (template: FactoryTemplate) => invoke<FactoryTemplate>("template_save", { template }),
  templateRemove: (id: number) => invoke<void>("template_remove", { id }),
  factorySettings: () => invoke<FactorySettings>("factory_settings"),
  powerStatus: () => invoke<PowerStatus>("power_status"),
  lifecycleStatus: () => invoke<LifecycleStatus>("lifecycle_status"),
  lifecycleTraySet: (enabled: boolean) => invoke<LifecycleStatus>("lifecycle_tray_set", { enabled }),
  lifecycleAutostartSet: (enabled: boolean) => invoke<LifecycleStatus>("lifecycle_autostart_set", { enabled }),
  lifecycleHide: () => invoke<void>("lifecycle_hide"),
  diagnosticsCollect: () => invoke<DiagnosticBundle>("diagnostics_collect"),
  diagnosticsExport: (path: string) => invoke<void>("diagnostics_export", { path }),
  diskSnapshot: () => invoke<DiskSnapshot>("disk_snapshot"),
  diskPolicySave: (policy: DiskPolicy) => invoke<DiskPolicy>("disk_policy_save", { policy }),
  diskCleanup: () => invoke<DiskCleanup>("disk_cleanup"),
  factorySettingsSet: (settings: FactorySettings) => invoke<FactorySettings>("factory_settings_set", { settings }),
  factoryState: () => invoke<FactoryState>("factory_state"),
  factoryItemUpdate: (id: number, priority: number, position: number, paused: boolean) => invoke<void>("factory_item_update", { id, priority, position, paused }),
  factoryDecide: (ids: number[], approve: boolean) => invoke<number>("factory_decide", { ids, approve }),
  nodeConfigGet: () => invoke<ServerConfig | null>("node_config_get"),
  nodeConfigSet: (config: ServerConfig) => invoke<void>("node_config_set", { config }),
  nodeFuseReset: () => invoke<void>("node_fuse_reset"),
  nodeOverview: () => invoke<NodeStatus>("node_overview"),
  nodeCheck: () => invoke<NodeStatus>("node_check"),
  nodeHostkeyScan: () => invoke<NodeHostKey>("node_hostkey_scan"),
  nodeHostkeyAccept: (line: string) => invoke<void>("node_hostkey_accept", { line }),
  nodeCandidates: () => invoke<NodeCandidate[]>("node_candidates"),
  nodeSend: (ids: number[]) => invoke<NodeSendReport>("node_send", { ids }),
  nodePull: () => invoke<NodePulled>("node_pull"),
  nodeRecall: (id: number) => invoke<void>("node_recall", { id }),
  nodeHandover: (ids: number[]) => invoke<NodeHandover>("node_handover", { ids }),
  nodePoweroff: () => invoke<void>("node_poweroff"),
  modesState: () => invoke<ModesState>("modes_state"),
  sceneActivate: (id: number) => invoke<FlowAction[]>("scene_activate", { id }),
  sceneUnpin: () => invoke<void>("scene_unpin"),
  sceneSave: (scene: SceneDto) => invoke<SceneDto>("scene_save", { scene }),
  sceneDelete: (id: number) => invoke<void>("scene_delete", { id }),
  ruleSave: (rule: RuleDto) => invoke<void>("rule_save", { rule }),
  ruleDelete: (id: number) => invoke<void>("rule_delete", { id }),
  appInfo: () => invoke<AppInfo>("app_info"),
  mediaUrl: (path: string) => invoke<string>("media_url", { path }),
  importVideo: (path: string) => invoke<Project>("import_video", { path }),
  listProjects: () => invoke<Project[]>("list_projects"),
  deleteProject: (id: number) => invoke<void>("delete_project", { id }),
  projectSetTight: (id: number, tight: boolean) => invoke<Project>("project_set_tight", { id, tight }),
  modelsStatus: () => invoke<ModelsStatus>("models_status"),
  modelsDownload: () => invoke<boolean>("models_download"),
  analysisStart: (projectId: number, options?: AnalysisOptions) =>
    invoke<void>("analysis_start", { projectId, options }),
  analysisOptionsGet: () => invoke<AnalysisSettings>("analysis_options_get"),
  analysisOptionsSet: (options: AnalysisOptions) => invoke<AnalysisSettings>("analysis_options_set", { options }),
  analysisCancel: (projectId: number) => invoke<void>("analysis_cancel", { projectId }),
  analysisInputsGet: (projectId: number) => invoke<AnalysisInputs>("analysis_inputs_get", { projectId }),
  analysisChatImport: (projectId: number, path: string) => invoke<AnalysisInputs>("analysis_chat_import", { projectId, path }),
  analysisChatClear: (projectId: number) => invoke<void>("analysis_chat_clear", { projectId }),
  analysisClipsClear: (projectId: number) => invoke<void>("analysis_clips_clear", { projectId }),
  analysisTwitchClips: (projectId: number, vodId: string) => invoke<AnalysisInputs>("analysis_twitch_clips", { projectId, vodId }),
  twitchSignalStatus: () => invoke<TwitchSignalStatus>("twitch_signal_status"),
  twitchSignalCredentials: (clientId: string, clientSecret: string) => invoke<void>("twitch_signal_credentials", { clientId, clientSecret }),
  analysisTimeline: (projectId: number) => invoke<AnalysisTimeline>("analysis_timeline", { projectId }),
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
  queueState: (projectId: number) => invoke<QueueState>("queue_state", { projectId }),
  queuePrepare: (projectId: number) => invoke<void>("queue_prepare", { projectId }),
  queuePrepareCancel: () => invoke<void>("queue_prepare_cancel"),
  queueApplyText: (projectId: number, caption: string, hashtags: string[], preserveHashtags = false) => invoke<number>("queue_apply_text", { projectId, caption, hashtags, preserveHashtags }),
  queueJobUpdate: (id: number, caption: string, hashtags: string[]) => invoke<PublishJob>("queue_job_update", { id, caption, hashtags }),
  queueJobResolve: (id: number, action: JobResolve) => invoke<void>("queue_job_resolve", { id, action }),
  queueJobAccount: (id: number, account: number | null) => invoke<void>("queue_job_account", { id, account }),
  readyAdd: (projectId: number, paths: string[]) => invoke<ReadyVideo[]>("ready_add", { projectId, paths }),
  readyRemove: (id: number) => invoke<void>("ready_remove", { id }),
  queueSettingsSet: (pauseMin: number, dailyLimit: number, windowStart: number, windowEnd: number) =>
    invoke<QueueSettings>("queue_settings_set", { pauseMin, dailyLimit, windowStart, windowEnd }),
  accountsAdd: (labels: string[]) => invoke<number>("accounts_add", { labels }),
  accountsSetEnabled: (id: number, enabled: boolean) => invoke<void>("accounts_set_enabled", { id, enabled }),
  accountsRemove: (id: number) => invoke<void>("accounts_remove", { id }),
  accountsOpenLogin: (id: number) => invoke<void>("accounts_open_login", { id }),
  queueStart: (projectId: number, confirmed: boolean) => invoke<void>("queue_start", { projectId, confirmed }),
  queuePause: () => invoke<void>("queue_pause"),
  queueStop: (projectId: number) => invoke<void>("queue_stop", { projectId }),
  youtubeAnalyze: (url: string) => invoke<YoutubeInfo>("youtube_analyze", { url }),
  youtubeDownload: (args: YoutubeStart) => invoke<string>("youtube_download", { args }),
  youtubeCancel: (jobId: string) => invoke<void>("youtube_cancel", { jobId }),
  batchList: () => invoke<BatchItem[]>("batch_list"),
  batchAdd: (items: BatchNewItem[]) => invoke<number>("batch_add", { items }),
  batchStart: () => invoke<void>("batch_start"),
  batchCancel: () => invoke<void>("batch_cancel"),
  batchRemove: (id: number) => invoke<void>("batch_remove", { id }),
  batchRetry: (id: number) => invoke<void>("batch_retry", { id }),
  batchApproveReady: () => invoke<number>("batch_approve_ready"),
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

// Очередь автовыкладки для просмотра экрана «Финал» в браузере.
const nowSec = () => Math.floor(Date.now() / 1000);
let demoJobs: PublishJob[] = [];
let demoGenerating = false;
let demoRunning = false;
let demoNextAt: number | null = null;
let demoQueueSettings: QueueSettings = { pauseMin: 45, dailyLimit: 5, windowStart: 0, windowEnd: 1440 };
let demoAccounts: AccountInfo[] = [{ id: 1, label: "Основной", enabled: true, main: true, sentToday: 0 }];
const demoQueueState = (): QueueState => ({
  jobs: demoJobs.map((j) => ({ ...j })),
  settings: demoQueueSettings,
  accounts: demoAccounts.map((a) => ({ ...a })),
  pool: demoAccounts.filter((a) => a.enabled).length > 1,
  running: demoRunning,
  generating: demoGenerating,
  sentToday: demoJobs.filter((j) => j.state === "done").length,
  nextAt: demoNextAt,
  note: null,
  windowMin: 0,
  windowMax: 3999,
  maxTags: 5,
  captionLimit: 4000,
  chromium: true,
});

let demoAnalysisSettings: AnalysisSettings = { ...defaultAnalysisSettings };
try {
  const saved = localStorage.getItem("demo-analysis-options");
  if (saved) demoAnalysisSettings = normalizeAnalysis(JSON.parse(saved));
} catch { /* Приватный режим браузера или повреждённые демо-настройки. */ }

function loadDemoFactory(): { channels: Channel[]; templates: FactoryTemplate[]; settings: FactorySettings } {
  try { const saved = localStorage.getItem("demo-factory"); if (saved) return JSON.parse(saved); } catch { /* Нет сохранённых демо-данных. */ }
  return { channels: [], templates: [{ id: 1, name: "По умолчанию", layout: "gameplay_stack", config: structuredClone(demoDefaults), isDefault: true }], settings: { draftConcurrency: 2, finalConcurrency: 1, resumeOnStart: true, inhibitSleep: true } };
}
const demoFactory = loadDemoFactory();
const persistDemoFactory = () => localStorage.setItem("demo-factory", JSON.stringify(demoFactory));

const demoTrendConfig: TrendConfig = { twitch: true, kick: false, youtube: false, topics: true, languages: ["ru"], categories: ["Dota 2"], keywords: ["турнир"], knownChannels: [], whitelist: [], threshold: 0.75, showFrom: 0.2, dailyLimit: 5, intervalMin: 10, region: "RU", gamesTop: 8 };
const demoTrends: TrendCandidate[] = [
  { id: 1, kind: "live", platform: "twitch", url: "https://twitch.tv/zxcursed", title: "ФИНАЛ турнира: решающая карта", category: "Dota 2", language: "ru", channelName: "ZXCursed", metric: 14200, previous: 3100, windowMin: 30, score: 0.91, why: "+358 % за 30 мин, категория Dota 2, совпала тема", state: "new", auto: false, lastSeen: 0 },
  { id: 2, kind: "clip", platform: "twitch", url: "https://clips.twitch.tv/Demo", title: "Невероятный камбэк на последних секундах", category: "Dota 2", language: "ru", channelName: "Bro", metric: 5200, previous: 900, windowMin: 40, score: 0.74, why: "+478 % за 40 мин, категория Dota 2", state: "new", auto: false, lastSeen: 0 },
  { id: 3, kind: "vod", platform: "youtube", url: "https://youtube.com/watch?v=demo", title: "Разбор главной новости недели", category: "25", language: "ru", channelName: "Новости", metric: 480000, previous: 0, windowMin: 0, score: 0.42, why: "", state: "new", auto: false, lastSeen: 0 },
];
const mock: typeof real = {
  nodeKeygen: async () => ({ keyPath: "~/.local/share/app/server_key", publicKey: "ssh-ed25519 AAAA…demo avtonarezka", version: "0.4.1", created: true }),
  integrationTest: async name => `${name}: проверка в демо-режиме не выполняется`,
  nodeConfigPush: async groups => ({ applied: groups, skipped: [] }),
  nodeConfigRemote: async () => ({ secrets: { factory_twitch_app: false, discovery_youtube_key: false, discovery_kick_token: false, notify_bot_token: false }, kv: {}, keyStore: true }),
  notifyState: async () => ({ config: { enabled: false, chat: "", summaryHour: 9, attention: true }, token: false }),
  notifySet: async () => undefined,
  notifyTest: async () => undefined,
  liveState: async () => ({ sessions: [{ id: 1, platform: "twitch", url: "https://twitch.tv/zxcursed", channel: "zxcursed", title: "", state: "live", manual: false, chat: true, startedAt: Math.floor(Date.now() / 1000) - 1500, endedAt: null, note: "", moments: 2 }, { id: 2, platform: "kick", url: "https://kick.com/abc", channel: "", title: "Слоты", state: "ended", manual: true, chat: false, startedAt: Math.floor(Date.now() / 1000) - 9000, endedAt: Math.floor(Date.now() / 1000) - 5400, note: "эфир закончился", moments: 1 }], moments: [{ id: 1, sessionId: 1, score: 8.4, manual: false, state: "cut", itemId: 5, error: null, createdAt: Math.floor(Date.now() / 1000) - 600, length: 62 }, { id: 2, sessionId: 1, score: 5.1, manual: false, state: "cut", itemId: 6, error: null, createdAt: Math.floor(Date.now() / 1000) - 200, length: 48 }, { id: 3, sessionId: 2, score: 0, manual: true, state: "failed", itemId: null, error: "Окно момента почти целиком вне кольца", createdAt: Math.floor(Date.now() / 1000) - 5500, length: 45 }], flowEnabled: true, maxSessions: 2, diskFreeMb: 48000 }),
  liveStart: async () => 3,
  liveStop: async () => undefined,
  liveCutNow: async () => undefined,
  liveMomentIgnore: async () => undefined,
  discoveryState: async () => ({ config: demoTrendConfig, new: demoTrends, watch: [], accepted: [], providers: { twitch: { configured: true, ok: true, at: 0, found: 58, message: "" }, kick: { configured: false, ok: null, at: 0, found: 0, message: "не настроено" }, youtube: { configured: false, ok: null, at: 0, found: 0, message: "не настроено" }, topics: { configured: true, ok: true, at: 0, found: 50, message: "" } }, topics: ["финал лиги", "курс доллара", "чемпионат мира"], lastPoll: Math.floor(Date.now() / 1000) - 240, flowEnabled: true, youtubeKey: false, kickToken: false, twitchKeys: true }),
  discoveryConfigSet: async c => c,
  discoverySecretSet: async () => undefined,
  discoveryPollNow: async () => ({ seen: 58, fresh: 3, shown: 3, autoAccepted: 0 }),
  discoveryAccept: async () => 1,
  discoveryReject: async () => undefined,
  modesState: async () => ({ node: "pc" as const, scenes: [], rules: [], active: null, pinnedUntil: 0, actions: [], log: [] }),
  sceneActivate: async () => [],
  sceneUnpin: async () => undefined,
  sceneSave: async (scene: SceneDto) => scene,
  sceneDelete: async () => undefined,
  ruleSave: async () => undefined,
  ruleDelete: async () => undefined,
  nodeConfigGet: async () => null,
  nodeConfigSet: async () => { throw new Error("Доступно в настольном приложении"); },
  nodeFuseReset: async () => undefined,
  nodeOverview: async () => ({ configured: false, enabled: false, config: null, fuse: { failures: 0, blocked: false, message: null }, online: false, error: null, remote: null }),
  nodeCheck: async () => { throw new Error("Связь с сервером доступна в настольном приложении"); },
  nodeHostkeyScan: async () => { throw new Error("Доступно в настольном приложении"); },
  nodeHostkeyAccept: async () => undefined,
  nodeCandidates: async () => [],
  nodeSend: async () => ({ sent: [], failed: [] }),
  nodePull: async () => ({ finished: [], failed: [], waiting: 0 }),
  nodeRecall: async () => undefined,
  nodeHandover: async () => ({ sent: [], failed: [], safeToPowerOff: false, staysHere: [] }),
  nodePoweroff: async () => undefined,
  diagnosticsCollect: async () => ({format: "avtonarezka-diagnostics-v1", createdAt: new Date().toISOString(), app: {version: "0.3.0", os: "browser-preview", arch: "preview", debug: true}, database: {schema: 16, foreignKeyViolations: 0, counts: {}, factoryStages: {}, publicationStates: {}, events: []}, runtimeLog: [], logWriteFailed: false, resources: {analyses: 0, renders: 0, downloads: 0, mlBusy: false, sourceBusy: false, inhibitActive: false, inhibitNeeded: false, inhibitFailed: false}, models: {coreReady: false, face: false}, tools: {}, privacy: "Предпросмотр интерфейса без подключения к приложению."}),
  diagnosticsExport: async () => { throw new Error("Экспорт диагностики доступен в приложении Автонарезка"); },
  diskSnapshot: async () => ({ policy: { automatic: false, keepDays: 14, maxGib: 100 }, files: [], channels: [], categories: [], managedBytes: 0, reclaimableBytes: 0, selectedBytes: 0, missing: 0, limitUnreachable: false }),
  diskPolicySave: async () => { throw new Error("Политика хранения настраивается в настольном приложении"); },
  diskCleanup: async () => { throw new Error("Очистка доступна в настольном приложении"); },
  powerStatus: async () => ({ active: false, needed: false, reason: "Доступно в настольном приложении", error: null }),
  lifecycleStatus: async () => ({ trayEnabled: false, trayActive: false, trayError: null, autostartAvailable: false, autostartEnabled: false, autostartError: null, windowVisible: true }),
  lifecycleTraySet: async () => { throw new Error("Трей доступен в настольном приложении"); },
  lifecycleAutostartSet: async () => { throw new Error("Автозапуск доступен в настольном приложении"); },
  lifecycleHide: async () => { throw new Error("Скрытие окна доступно в настольном приложении"); },
  notificationsEnabled: async () => false,
  notificationsSet: async () => { throw new Error("Уведомления настраиваются в настольном приложении"); },
  analyticsDashboard: async (): Promise<AnalyticsDashboard> => ({ rows: [], published: 0, measured: 0, views: null, medianViews: null, errors: 0, attention: 0, errorRate: null, hours: Array.from({ length: 24 }, (_, hour) => ({ hour, published: 0, medianViews: null })), autoCollect: false, checkedAt: null }),
  analyticsHistory: async (): Promise<MetricSample[]> => [],
  analyticsManual: async () => { throw new Error("Замеры сохраняются в настольном приложении"); },
  analyticsCollect: async () => { throw new Error("Сбор метрик доступен в настольном приложении"); },
  analyticsAuto: async () => { throw new Error("Настройки сохраняются в настольном приложении"); },
  analyticsFeedback: async (channelId: number): Promise<AnalyticsFeedback> => ({ channelId, clips: 0, reason: "Нужно 50 разных роликов с замерами в 48–54 часа после публикации.", suggestions: [] }),
  analyticsApply: async () => { throw new Error("Подсказки применяются в настольном приложении"); },
  campaignReportOpen: async () => { throw new Error("Просмотр отчёта доступен в настольном приложении"); },
  campaignReportExport: async () => { throw new Error("Экспорт доступен в настольном приложении"); },
  campaignsList: async () => [],
  campaignSave: async () => { throw new Error("Кампании сохраняются в настольном приложении"); },
  campaignPreview: async () => { throw new Error("Ротация доступна в настольном приложении"); },
  campaignAssignments: async () => [],
  layoutPreview: async () => { throw new Error("Предпросмотр доступен в настольном приложении"); },
  publicationAccounts: async () => [],
  vkMetricsOptions: async (): Promise<VkMetricsOptions> => ({ accounts: [], clips: [], secretBackend: "memory" }),
  vkMetricsAccountSave: async () => { throw new Error("Подключение VK доступно в настольном приложении"); },
  vkMetricsAccountEnabled: async () => { throw new Error("Настройки VK доступны в настольном приложении"); },
  vkMetricsLink: async () => { throw new Error("Привязка VK доступна в настольном приложении"); },
  publicationAccountPolicy: async () => {},
  instagramAccountSave: (args: InstagramAccountArgs) => invoke<number>("instagram_account_save", { args }),
  telegramAccountSave: async () => { throw new Error("Подключение Telegram доступно в настольном приложении"); },
  telegramAccountCheck: async () => { throw new Error("Проверка Telegram доступна в настольном приложении"); },
  youtubeOAuthSettings: async () => ({ clientId: "", hasSecret: false, secretBackend: "memory", loginRunning: false }),
  youtubeOAuthSave: async () => { throw new Error("Подключение YouTube доступно в настольном приложении"); },
  youtubeLoginStart: async () => { throw new Error("Подключение YouTube доступно в настольном приложении"); },
  youtubeLoginCancel: async () => {},
  youtubeAccountSave: async () => { throw new Error("Настройки YouTube доступны в настольном приложении"); },
  publicationsList: async () => ({ publications: [], running: false, note: null, secretBackend: "memory" }),
  publicationAutofill: async () => { throw new Error("Автозаполнение доступно в настольном приложении"); },
  publicationSchedule: async () => { throw new Error("Расписание доступно в настольном приложении"); },
  publicationMove: async () => { throw new Error("Расписание доступно в настольном приложении"); },
  publicationRefreshCaption: async () => { throw new Error("Обновление подписи доступно в настольном приложении"); },
  publicationCancel: async () => { throw new Error("Расписание доступно в настольном приложении"); },
  publicationResolve: async () => { throw new Error("Проверка публикаций доступна в настольном приложении"); },
  publicationsStart: async () => { throw new Error("Публикация доступна в настольном приложении"); },
  factoryCalendar: async () => [],
  platformCaps: async () => [{ platform: "tiktok", name: "TikTok", captionMax: 3999, titleMax: 0, hashtagsMax: 5, durationMax: null, fileMax: null, dailyDefault: 5, nativeSchedule: false, login: "oauth_or_browser" }, { platform: "telegram", name: "Telegram", captionMax: 1024, titleMax: 0, hashtagsMax: 30, durationMax: null, fileMax: 50_000_000, dailyDefault: 10, nativeSchedule: false, login: "bot_token" }, { platform: "youtube", name: "YouTube Shorts", captionMax: 5000, titleMax: 100, hashtagsMax: 15, durationMax: 180, fileMax: 256_000_000_000, dailyDefault: 5, nativeSchedule: true, login: "google_oauth" }],
  outputProfiles: async () => (["tiktok", "telegram", "youtube", "instagram"] as const).map(platform => ({ platform, limits: { maxSeconds: platform === "youtube" ? 180 : platform === "instagram" ? 900 : null, videoKbps: 6000, audioKbps: 128 } })),
  outputProfileSave: async () => { throw new Error("Выходные профили доступны в настольном приложении"); },
  outputPreview: async () => { throw new Error("Рендер варианта доступен в настольном приложении"); },
  captionDraftLoad: async (draftId, platform) => { const d = [...demoDrafts, ...demoFinals].find(d => d.id === draftId); return { spec: { platform, template: "{название}\n\n{тело}\n\n{кредит}\n{ссылка}\n{промокод}\n{хэштеги}" }, fields: { title: platform === "youtube" ? d?.title ?? "" : "", body: "", credit: "", link: "", promo: "", hashtags: d?.hashtags ?? [] } }; },
  captionDraftSave: async () => { throw new Error("Проверка и сохранение подписи доступны в настольном приложении"); },
  captionAccountLoad: async (draftId, platform) => mock.captionDraftLoad(draftId, platform),
  captionAccountSave: async () => { throw new Error("Сохранение подписи доступно в настольном приложении"); },
  captionApplyBody: async () => { throw new Error("Сохранение текста доступно в настольном приложении"); },
  captionDraftGenerate: async () => { throw new Error("Локальная модель доступна в настольном приложении"); },
  accountsList: async () => demoAccounts.map(a => ({ ...a })),
  channelsList: async () => demoFactory.channels.map(c => ({ ...structuredClone(c), analysis: normalizeAnalysis(c.analysis), monitor: { ...defaultMonitorOptions, ...c.monitor }, lastCheckedAt: c.lastCheckedAt ?? null, nextCheckAt: c.nextCheckAt ?? null, sourceError: c.sourceError ?? null, complaintLimit: c.complaintLimit ?? 3, rightsPaused: c.rightsPaused ?? false, activeIncidents: c.activeIncidents ?? 0 })),
  cameraFrame: async () => { throw new Error("Кадр камеры доступен в настольном приложении на импортированном видео"); },
  cameraDetect: async () => { throw new Error("Поиск лица доступен в настольном приложении"); },
  cameraScan: async () => { throw new Error("Проверка записи доступна в настольном приложении"); },
  cameraScanCancel: async () => {},
  faceModelReady: async () => false,
  faceModelDownload: async () => { throw new Error("Загрузка YuNet доступна в настольном приложении"); },
  sourceCheck: async () => { throw new Error("Проверка источников доступна в настольном приложении"); },
  channelIncidents: async () => [],
  channelIncidentAdd: async () => { throw new Error("Учёт жалоб доступен в настольном приложении"); },
  channelIncidentResolve: async () => { throw new Error("Учёт жалоб доступен в настольном приложении"); },
  channelRightsResume: async () => { throw new Error("Возобновление доступно в настольном приложении"); },
  channelSave: async (channel) => {
    if (!channel.name.trim()) throw new Error("Укажи название канала");
    if (channel.monitor.minDuration !== null && channel.monitor.maxDuration !== null && channel.monitor.minDuration > channel.monitor.maxDuration) throw new Error("Максимальная длительность меньше минимальной");
    if (channel.monitor.intervalMinutes < 1 || channel.monitor.intervalMinutes > 1440) throw new Error("Интервал проверки: от 1 до 1440 минут");
    const saved = { ...structuredClone(channel), id: channel.id || Math.max(0, ...demoFactory.channels.map(c => c.id)) + 1 };
    demoFactory.channels = [...demoFactory.channels.filter(c => c.id !== saved.id), saved]; persistDemoFactory(); return saved;
  },
  channelRemove: async (id) => { demoFactory.channels = demoFactory.channels.filter(c => c.id !== id); persistDemoFactory(); },
  templatesList: async () => structuredClone(demoFactory.templates),
  templateSave: async (template) => {
    if (!template.name.trim()) throw new Error("Укажи название шаблона");
    const saved = { ...structuredClone(template), id: template.id || Math.max(0, ...demoFactory.templates.map(t => t.id)) + 1, isDefault: template.id === 1 };
    demoFactory.templates = [...demoFactory.templates.filter(t => t.id !== saved.id), saved]; persistDemoFactory(); return saved;
  },
  templateRemove: async (id) => {
    if (id === 1 || demoFactory.channels.some(c => c.templateId === id)) throw new Error("Шаблон по умолчанию или используется каналом");
    demoFactory.templates = demoFactory.templates.filter(t => t.id !== id); persistDemoFactory();
  },
  factorySettings: async () => ({ ...demoFactory.settings }),
  factorySettingsSet: async (settings) => { demoFactory.settings = settings; persistDemoFactory(); return settings; },
  factoryState: async () => {
    const saved = localStorage.getItem("demo-factory-state");
    return saved ? JSON.parse(saved) as FactoryState : { items: [], events: [], inbox: [], running: false, paused: false };
  },
  factoryItemUpdate: async () => {},
  factoryDecide: async (ids, approve) => {
    const saved = localStorage.getItem("demo-factory-state"); if (!saved) return 0;
    const state: FactoryState = JSON.parse(saved);
    const n = state.inbox.filter(i => ids.includes(i.draft.id) && (!approve || i.draft.status === "ready")).length;
    state.inbox = state.inbox.filter(i => !ids.includes(i.draft.id) || (approve && i.draft.status !== "ready"));
    localStorage.setItem("demo-factory-state", JSON.stringify(state)); return n;
  },
  appInfo: async () => ({
    version: "dev",
    data_dir: "",
    env: { os: "linux", ffmpeg: "ffmpeg", nvenc: true, libass: true, ytdlp: "2026.10.26", ytdlpOk: true, twitchDownloader: null, jsRuntime: "deno" },
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
  modelsStatus: async () => ({ dir: "~/.local/share/…/models", whisper: true, yamnet: true, llm: true, worker: true, ready: true, downloading: false }),
  modelsDownload: async () => false,
  analysisOptionsGet: async () => structuredClone(demoAnalysisSettings),
  analysisOptionsSet: async (options) => {
    demoAnalysisSettings = normalizeAnalysis(options);
    localStorage.setItem("demo-analysis-options", JSON.stringify(demoAnalysisSettings));
    return structuredClone(demoAnalysisSettings);
  },
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
  analysisInputsGet: async () => ({ chat: null, clips: null, chatError: null }),
  analysisChatImport: async () => { throw new Error("Импорт чата доступен в настольном приложении"); },
  analysisChatClear: async () => {},
  analysisClipsClear: async () => {},
  analysisTwitchClips: async () => { throw new Error("Запрос Twitch доступен в настольном приложении"); },
  twitchSignalStatus: async () => ({ configured: false, storage: "memory" }),
  twitchSignalCredentials: async () => { throw new Error("Секреты сохраняются только в настольном приложении"); },
  analysisTimeline: async () => ({ duration: 0, hop: 0.5, series: [], scout: null }),
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
  queueState: async () => demoQueueState(),
  queuePrepare: async (projectId) => {
    demoGenerating = true;
    demoJobs = demoFinals.map((d, i) => ({ id: 300 + i, draftId: d.id, readyId: null, name: "", wantedAccount: null, caption: "", hashtags: [], state: "drafting" as JobState, error: null, attempts: 0, notBefore: 0, len: 0, updatedAt: nowSec() }));
    mockBus.dispatchEvent(new CustomEvent("queue-update", { detail: { projectId } }));
    demoJobs.forEach((j, i) => {
      for (const [k, message] of ["Подбираю хэштеги"].entries()) {
        setTimeout(() => mockBus.dispatchEvent(new CustomEvent("queue-text", { detail: { draftId: j.draftId, message } })), 500 + i * 1800 + k * 500);
      }
      setTimeout(() => {
        const tags = ["юмор", "сериал", "тикток", "момент", "смех"];
        // Текст вводится вручную; модель подбирает только теги.
        j.hashtags = tags.map((t) => `#${t}`);
        j.len = captionLen(j.caption, j.hashtags);
        j.state = "ready";
        demoGenerating = demoJobs.some((x) => x.state === "drafting");
        mockBus.dispatchEvent(new CustomEvent("queue-update", { detail: { projectId } }));
      }, 2200 + i * 1800);
    });
  },
  queuePrepareCancel: async () => {
    demoGenerating = false;
  },
  queueApplyText: async (projectId, caption, hashtags, preserveHashtags = false) => {
    const tags = normalizeTags(hashtags);
    const len = captionLen(caption, tags);
    if (tags.length > 5) throw "Хэштегов должно быть не больше 5";
    if (len > 4000) throw `Подпись с хэштегами ${len} знаков, а страница TikTok принимает не больше 4000`;
    const busy: JobState[] = ["drafting", "queued", "sending", "done", "needs_check"];
    const eligible = demoFinals.filter((d) => !demoJobs.some((j) => j.draftId === d.id && busy.includes(j.state)));
    if (eligible.length === 0) throw "Нет роликов, которым можно назначить текст: все уже в очереди, выложены или ждут проверки";
    for (const d of eligible) {
      const ownTags = preserveHashtags ? (demoJobs.find(j => j.draftId === d.id)?.hashtags ?? []) : tags;
      if (captionLen(caption, ownTags) > 4000) throw "Текст вместе с хэштегами одного из роликов превышает 4000 знаков";
    }
    for (const d of eligible) {
      const old = demoJobs.find((j) => j.draftId === d.id);
      const ownTags = preserveHashtags ? (old?.hashtags ?? []) : tags;
      const fields = { caption: caption.trim(), hashtags: ownTags, len: captionLen(caption, ownTags), state: "ready" as JobState, error: null };
      if (old) Object.assign(old, fields);
      else demoJobs.push({ id: 300 + demoJobs.length + 1, draftId: d.id, readyId: null, name: "", wantedAccount: null, attempts: 0, notBefore: 0, updatedAt: nowSec(), ...fields });
    }
    mockBus.dispatchEvent(new CustomEvent("queue-update", { detail: { projectId } }));
    return eligible.length;
  },
  queueJobUpdate: async (id, caption, hashtags) => {
    const j = demoJobs.find((x) => x.id === id);
    if (!j) throw "Задача не найдена";
    const len = captionLen(caption, hashtags);
    if (len > 4000) throw `Подпись с хэштегами ${len} знаков, а страница TikTok принимает не больше 4000`;
    Object.assign(j, { caption: caption.trim(), hashtags: normalizeTags(hashtags), len, state: "ready" });
    return j;
  },
  queueJobAccount: async () => {},
  readyAdd: async (_projectId, paths) => paths.map((p, i) => ({ id: i + 1, name: p.split(/[\\/]/).pop() ?? p, duration: 60, width: 1080, height: 1920, warning: null })),
  readyRemove: async () => {},
  queueJobResolve: async (id, action) => {
    const j = demoJobs.find((x) => x.id === id);
    if (!j) return;
    if (action === "remove") demoJobs = demoJobs.filter((x) => x.id !== id);
    else Object.assign(j, { state: action === "retry" ? "queued" : "done", error: null });
  },
  queueSettingsSet: async (pauseMin, dailyLimit, windowStart, windowEnd) =>
    (demoQueueSettings = {
      pauseMin: Math.max(1, pauseMin),
      dailyLimit: Math.max(1, dailyLimit),
      windowStart: Math.min(1440, Math.max(0, windowStart)),
      windowEnd: Math.min(1440, Math.max(0, windowEnd)),
    }),
  accountsAdd: async (labels) => {
    let added = 0;
    for (const raw of labels) {
      const label = raw.trim();
      if (!label || demoAccounts.some((a) => a.label.toLowerCase() === label.toLowerCase())) continue;
      demoAccounts.push({ id: Math.max(0, ...demoAccounts.map((a) => a.id)) + 1, label, enabled: true, main: false, sentToday: 0 });
      added++;
    }
    return added;
  },
  accountsSetEnabled: async (id, enabled) => {
    const a = demoAccounts.find((x) => x.id === id);
    if (a) a.enabled = enabled;
  },
  accountsRemove: async (id) => {
    demoAccounts = demoAccounts.filter((x) => x.id !== id);
  },
  accountsOpenLogin: async () => {},
  queueStart: async (projectId, confirmed) => {
    if (!confirmed) throw "Нужно подтверждение запуска";
    demoJobs.filter((j) => j.state === "ready").forEach((j) => (j.state = "queued"));
    demoRunning = true;
    const step = () => {
      const next = demoJobs.find((j) => j.state === "queued");
      if (!demoRunning || !next) {
        demoRunning = false;
        demoNextAt = null;
        mockBus.dispatchEvent(new CustomEvent("queue-update", { detail: { projectId } }));
        return;
      }
      next.state = "sending";
      demoNextAt = null;
      mockBus.dispatchEvent(new CustomEvent("queue-update", { detail: { projectId } }));
      setTimeout(() => {
        next.state = "done";
        demoNextAt = nowSec() + 20;
        mockBus.dispatchEvent(new CustomEvent("queue-update", { detail: { projectId } }));
        setTimeout(step, 20000);
      }, 2500);
    };
    step();
  },
  queuePause: async () => {
    demoRunning = false;
    demoNextAt = null;
  },
  queueStop: async () => {
    demoRunning = false;
    demoNextAt = null;
    demoJobs.filter((j) => j.state === "queued").forEach((j) => (j.state = "ready"));
  },
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
  batchList: async () => [],
  batchAdd: async (items) => items.length,
  batchStart: async () => {},
  batchCancel: async () => {},
  batchRemove: async () => {},
  batchRetry: async () => {},
  batchApproveReady: async () => 0,
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

export interface DiagnosticBundle {
  format: string; createdAt: string;
  app: {version: string; os: string; arch: string; debug: boolean};
  database: {schema: number; foreignKeyViolations: number; counts: Record<string, number>; factoryStages: Record<string, number>; publicationStates: Record<string, number>; events: {id: number; at: number; kind: string; class: string}[]};
  runtimeLog: {at: number; level: string; component: string; class: string; fingerprint: string}[];
  logWriteFailed: boolean;
  resources: {analyses: number; renders: number; downloads: number; mlBusy: boolean; sourceBusy: boolean; inhibitActive: boolean; inhibitNeeded: boolean; inhibitFailed: boolean};
  models: {coreReady: boolean; face: boolean};
  tools: Record<string, boolean>;
  privacy: string;
}
