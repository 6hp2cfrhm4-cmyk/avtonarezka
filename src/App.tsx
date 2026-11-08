import { useEffect } from "react";
import { AnimatePresence, motion, type Variants } from "motion/react";
import Scene from "./scene/Scene";
import Stepper from "./components/Stepper";
import TopBar from "./components/TopBar";
import Toasts from "./components/Toasts";
import ErrorBoundary from "./components/ErrorBoundary";
import ImportScreen from "./screens/Import";
import AnalysisScreen from "./screens/Analysis";
import MomentsScreen from "./screens/Moments";
import DraftsScreen from "./screens/Drafts";
import FinalScreen from "./screens/Final";
import { onEvent } from "./lib/events";
import { listen } from "@tauri-apps/api/event";
import { api, errText, inTauri, type DoneEvent, type ErrorEvent, type ProgressEvent, type LoginEvent, type PostDoneEvent, type PostFailedEvent, type PostProgressEvent, type QueueTextEvent, type RenderDoneEvent, type RenderFailedEvent, type RenderProgressEvent, type YoutubeDoneEvent, type YoutubeErrorEvent, type YoutubeProgressEvent } from "./lib/api";
import { SCREENS, useUi, type ScreenId } from "./store";

/** Экраны с сеткой карточек. Их не крутим в 3D: целый прокручиваемый экран в 3D-слое с перспективой для композитора лишняя нагрузка. */
const HEAVY: ScreenId[] = ["drafts", "final"];

/** `d` — направление (1 вперёд, -1 назад), `heavy` — переход на тяжёлый экран: только прозрачность и небольшой сдвиг. */
type PageCustom = { d: number; heavy: boolean };

const page: Variants = {
  enter: ({ d, heavy }: PageCustom) => (heavy ? { opacity: 0, x: d * 60 } : { opacity: 0, rotateY: d * 26, x: d * 140, z: -280, scale: 0.94 }),
  center: { opacity: 1, rotateY: 0, x: 0, z: 0, scale: 1 },
  exit: ({ d, heavy }: PageCustom) => ({
    ...(heavy ? { opacity: 0, x: -d * 60 } : { opacity: 0, rotateY: -d * 26, x: -d * 140, z: -280, scale: 0.94 }),
    transition: { duration: 0.24, ease: [0.4, 0, 1, 1] },
  }),
};

function Screen({ id }: { id: ScreenId }) {
  if (id === "import") return <ImportScreen />;
  if (id === "analysis") return <AnalysisScreen />;
  if (id === "moments") return <MomentsScreen />;
  if (id === "drafts") return <DraftsScreen />;
  return <FinalScreen />;
}

export default function App() {
  const screen = useUi((s) => s.screen);
  const dir = useUi((s) => s.dir);
  const lite = useUi((s) => s.lite);
  const accent = useUi((s) => s.accent);
  const setInfo = useUi((s) => s.setInfo);
  const setProjects = useUi((s) => s.setProjects);
  const notify = useUi((s) => s.notify);
  const heavy = HEAVY.includes(screen);
  const custom: PageCustom = { d: dir, heavy };

  useEffect(() => {
    document.documentElement.style.setProperty("--accent", accent);
  }, [accent]);

  // События анализа: прогресс, готово, ошибка.
  useEffect(() => {
    const ui = () => useUi.getState();
    const offs = [
      onEvent<ProgressEvent>("analysis-progress", (p) => {
        if (p.projectId === ui().project?.id) ui().setAnalysis({ running: true, stage: p.stage, fraction: p.fraction, message: p.message });
      }),
      onEvent<DoneEvent>("analysis-done", async (d) => {
        ui().setAnalysis({ running: false, stage: "done", fraction: 1, notes: d.notes });
        const projects = await api.listProjects();
        ui().setProjects(projects);
        const fresh = projects.find((p) => p.id === d.projectId);
        if (fresh && ui().project?.id === fresh.id) {
          ui().setProject(fresh);
          ui().setMoments(await api.momentsList(fresh.id));
        }
        ui().notify(`Анализ готов: найдено моментов — ${d.moments}`);
        if (ui().screen === "analysis" && d.notes.length === 0) ui().go("moments");
      }),
      onEvent<ErrorEvent>("analysis-error", (e) => {
        ui().setAnalysis({ running: false });
        ui().notify(e.cancelled ? "Анализ отменён" : e.message, e.cancelled ? "info" : "error");
      }),
    ];
    return () => offs.forEach((f) => f());
  }, []);

  // Отладка: `curl 127.0.0.1:<порт>/dev/go?screen=moments` переключает экран (только debug-сборка).
  useEffect(() => {
    if (!import.meta.env.DEV || !inTauri) return;
    let dead = false;
    const offs: (() => void)[] = [];
    listen<ScreenId>("dev-go", (e) => useUi.getState().go(e.payload)).then((f) => (dead ? f() : offs.push(f)));
    listen<number>("dev-select", async (e) => {
      const projects = await api.listProjects();
      useUi.getState().setProjects(projects);
      useUi.getState().setProject(projects.find((p) => p.id === e.payload) ?? null);
      useUi.getState().setAssets(await api.assetsList());
      useUi.getState().setDefaults(await api.defaultsGet());
    }).then((f) => (dead ? f() : offs.push(f)));
    return () => {
      dead = true;
      offs.forEach((f) => f());
    };
  }, []);

  useEffect(() => {
    api.appInfo().then(setInfo).catch((e) => notify(errText(e), "error"));
    api.listProjects().then(setProjects).catch((e) => notify(errText(e), "error"));
    api.assetsList().then(useUi.getState().setAssets).catch((e) => notify(errText(e), "error"));
    api.defaultsGet().then(useUi.getState().setDefaults).catch((e) => notify(errText(e), "error"));
  }, [setInfo, setProjects, notify]);

  // События TikTok: вход и отправка роликов.
  useEffect(() => {
    const ui = () => useUi.getState();
    const refresh = async () => {
      const p = ui().project;
      if (p) ui().setPosts(await api.postsList(p.id));
    };
    const offs = [
      onEvent<LoginEvent>("tiktok-login", async (e) => {
        if (e.state !== "waiting") ui().notify(e.message, e.state === "error" ? "error" : "info");
        ui().setTikTok(await api.tiktokStatus());
      }),
      onEvent<PostProgressEvent>("post-progress", (e) => ui().setPostProgress(e.draftId, { fraction: e.fraction, message: e.message })),
      onEvent<PostDoneEvent>("post-done", async (e) => {
        ui().setPostProgress(e.draftId, null);
        ui().notify(e.detail);
        await refresh();
      }),
      onEvent<PostFailedEvent>("post-error", async (e) => {
        ui().setPostProgress(e.draftId, null);
        ui().notify(e.message, "error");
        await refresh();
      }),
    ];
    return () => offs.forEach((f) => f());
  }, []);

  // Очередь автовыкладки: любое изменение — перечитываем состояние (оно небольшое), ход написания описаний — в карточки.
  useEffect(() => {
    const ui = () => useUi.getState();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const reload = () => {
      clearTimeout(timer);
      timer = setTimeout(async () => {
        const p = ui().project;
        if (p) ui().setQueue(await api.queueState(p.id).catch(() => ui().queue));
      }, 120);
    };
    const offs = [
      onEvent<{ projectId?: number }>("queue-update", reload),
      onEvent<QueueTextEvent>("queue-text", (e) => ui().setQueueText(e.draftId, e.message)),
    ];
    return () => {
      clearTimeout(timer);
      offs.forEach((f) => f());
    };
  }, []);

  // События загрузки с YouTube: ход, готово, ошибка или остановка.
  useEffect(() => {
    const ui = () => useUi.getState();
    const offs = [
      onEvent<YoutubeProgressEvent>("youtube-progress", (e) => ui().upsertDownload(e)),
      onEvent<YoutubeDoneEvent>("youtube-done", async (d) => {
        ui().patchDownload(d.jobId, { status: "done", stage: "importing", fraction: 1, project: d.project });
        ui().setProjects(await api.listProjects());
        // как после выбора файла: проект становится выбранным, но экран не меняется.
        // Если человек сейчас работает с другим видео, его не трогаем
        if (ui().screen === "import") ui().setProject(d.project);
        ui().notify(`Скачано и добавлено в проекты: ${d.project.title}`);
        setTimeout(() => ui().removeDownload(d.jobId), 8000);
      }),
      onEvent<YoutubeErrorEvent>("youtube-error", (e) => ui().patchDownload(e.jobId, { status: e.cancelled ? "stopped" : "error", error: e.message })),
    ];
    return () => offs.forEach((f) => f());
  }, []);

  // События рендера: прогресс на карточках, обновление списка по окончании.
  useEffect(() => {
    const ui = () => useUi.getState();
    // Сотня роликов заканчивается сотней событий. Список перечитываем не на каждое, а раз в ~0,3 с
    // (но не реже чем раз в 1,5 с), иначе каждое событие тянет весь список и перерисовывает все карточки.
    let timer: ReturnType<typeof setTimeout> | undefined;
    let pendingSince = 0;
    const run = async () => {
      pendingSince = 0;
      const p = ui().project;
      if (p) ui().setDrafts(await api.draftsList(p.id).catch(() => ui().drafts));
    };
    const reload = () => {
      const now = Date.now();
      if (!pendingSince) pendingSince = now;
      clearTimeout(timer);
      timer = setTimeout(() => void run(), Math.min(300, Math.max(0, pendingSince + 1500 - now)));
    };
    const offs = [
      onEvent<RenderProgressEvent>("render-progress", (e) => ui().setRenderProgress(e.draftId, e.fraction)),
      onEvent<RenderDoneEvent>("render-done", (e) => {
        ui().setRenderProgress(e.draftId, null);
        reload();
      }),
      onEvent<RenderFailedEvent>("render-error", (e) => {
        ui().setRenderProgress(e.draftId, null);
        if (!e.cancelled) ui().notify(e.message, "error");
        reload();
      }),
    ];
    return () => {
      clearTimeout(timer);
      offs.forEach((f) => f());
    };
  }, []);

  return (
    <div className={`relative h-full ${lite ? "lite" : ""}`}>
      <Scene />
      <div className="relative z-10 flex h-full flex-col gap-6 p-7">
        <TopBar />
        <div className="flex min-h-0 flex-1 gap-8">
          <aside className="w-[230px] shrink-0 pt-2">
            <Stepper />
          </aside>
          <main className="relative min-w-0 flex-1" style={heavy ? undefined : { perspective: 1800 }}>
            <AnimatePresence mode="wait" custom={custom} initial={false}>
              <motion.div
                key={screen}
                custom={custom}
                variants={page}
                initial="enter"
                animate="center"
                exit="exit"
                transition={{ type: "spring", stiffness: 150, damping: 21, mass: 0.9 }}
                className="absolute inset-0 overflow-y-auto overflow-x-hidden pr-1"
                style={heavy ? undefined : { transformStyle: "preserve-3d" }}
                aria-label={SCREENS.find((s) => s.id === screen)?.label}
              >
                <ErrorBoundary resetKey={screen}>
                  <Screen id={screen} />
                </ErrorBoundary>
              </motion.div>
            </AnimatePresence>
          </main>
        </div>
      </div>
      <Toasts />
    </div>
  );
}
