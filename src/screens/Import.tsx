import { useCallback, useEffect, useRef, useState } from "react";
import { AnimatePresence, motion, useMotionValue, useSpring, useTransform } from "motion/react";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { listen } from "@tauri-apps/api/event";
import { api, errText, fmtDuration, inTauri, type AssetKind, type Project } from "../lib/api";
import AssetLibrary, { addAssets, AUDIO_EXT, VIDEO_EXT } from "../components/AssetLibrary";
import DownloadsList from "../components/DownloadsList";
import YoutubeCard from "../components/YoutubeCard";
import { extractYoutubeLink } from "../lib/youtube";
import { useUi } from "../store";
import { Arrow, Film, Link, Trash, Upload } from "../components/icons";
import { Switch } from "../components/Controls";

const EXT = ["mp4", "mov", "mkv", "webm", "avi", "m4v", "ts", "mpg", "mpeg"];
const isVideo = (p: string) => EXT.includes(p.split(".").pop()?.toLowerCase() ?? "");

type Mode = "file" | "youtube";

export default function ImportScreen() {
  const { project, projects, setProject, setProjects, notify, go } = useUi();
  const [dragging, setDragging] = useState(false);
  const [hoverKind, setHoverKind] = useState<AssetKind | null>(null);
  const [busy, setBusy] = useState(false);
  const [mode, setMode] = useState<Mode>("file");
  // ссылка, вставленная через Ctrl+V вне поля: карточка YouTube проверяет её сама
  const [paste, setPaste] = useState<{ url: string; n: number } | null>(null);

  const importPath = useCallback(
    async (path: string) => {
      setBusy(true);
      try {
        const p = await api.importVideo(path);
        setProject(p);
        setProjects(await api.listProjects());
      } catch (e) {
        notify(errText(e), "error");
      } finally {
        setBusy(false);
      }
    },
    [setProject, setProjects, notify],
  );

  // Перетаскивание файлов: Tauri отдаёт настоящие пути, а не File-объекты.
  useEffect(() => {
    if (!inTauri) return;
    let off: (() => void) | undefined;
    let dead = false;
    getCurrentWebview()
      .onDragDropEvent((e) => {
        const t = e.payload.type;
        // файл над колонкой библиотеки — это ассет, в остальных местах — новое видео
        const kindAt = (): AssetKind | null => {
          if (t === "leave" || !("position" in e.payload)) return null;
          const { x, y } = e.payload.position;
          const el = document.elementFromPoint(x / devicePixelRatio, y / devicePixelRatio);
          return ((el?.closest("[data-asset-kind]") as HTMLElement | null)?.dataset.assetKind as AssetKind | undefined) ?? null;
        };
        if (t === "enter" || t === "over") {
          const k = kindAt();
          setHoverKind(k);
          setDragging(k === null);
        } else if (t === "leave") {
          setDragging(false);
          setHoverKind(null);
        } else if (t === "drop") {
          const k = kindAt();
          setDragging(false);
          setHoverKind(null);
          if (k) {
            const ext = k === "music" ? AUDIO_EXT : VIDEO_EXT;
            const files = e.payload.paths.filter((p) => ext.includes(p.split(".").pop()?.toLowerCase() ?? ""));
            if (files.length) void addAssets(k, files);
            else notify(k === "music" ? "Для музыки подойдут mp3, wav, m4a, ogg, flac" : "Это не видеофайл. Подойдут mp4, mov, mkv, webm.", "error");
          } else {
            const file = e.payload.paths.find(isVideo);
            if (file) void importPath(file);
            else notify("Это не видеофайл. Подойдут mp4, mov, mkv, webm.", "error");
          }
        }
      })
      .then((f) => (dead ? f() : (off = f)));
    return () => {
      dead = true;
      off?.();
    };
  }, [importPath, notify]);

  // Ctrl+V на экране: если в буфере ссылка на YouTube, открываем вкладку и сразу проверяем её.
  useEffect(() => {
    let handled = false;
    const take = (text: string): boolean => {
      const link = extractYoutubeLink(text);
      if (!link) return false;
      setMode("youtube");
      setPaste({ url: link, n: Date.now() });
      return true;
    };
    const typing = (t: EventTarget | null) => t instanceof HTMLElement && (t.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(t.tagName));
    const onPaste = (e: ClipboardEvent) => {
      handled = true;
      if (typing(e.target)) return;
      if (take(e.clipboardData?.getData("text") ?? "")) e.preventDefault();
    };
    // Запасной путь: если окно само не прислало paste (бывает без поля ввода в фокусе), читаем буфер по Ctrl+V.
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.key.toLowerCase() !== "v" || typing(e.target)) return;
      handled = false;
      setTimeout(() => {
        if (handled) return;
        navigator.clipboard?.readText().then(take).catch(() => {});
      }, 120);
    };
    document.addEventListener("paste", onPaste);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("paste", onPaste);
      document.removeEventListener("keydown", onKey);
    };
  }, []);

  // Отладка: `curl 127.0.0.1:<порт>/dev/yt_tab?url=…` открывает вкладку YouTube (только debug-сборка).
  useEffect(() => {
    if (!import.meta.env.DEV || !inTauri) return;
    let dead = false;
    let off: (() => void) | undefined;
    listen<string>("dev-yt", (e) => {
      setMode("youtube");
      if (e.payload) setPaste({ url: e.payload, n: Date.now() });
    }).then((f) => (dead ? f() : (off = f)));
    return () => {
      dead = true;
      off?.();
    };
  }, []);

  const pick = async () => {
    const sel = await open({ multiple: false, filters: [{ name: "Видео", extensions: EXT }] });
    if (typeof sel === "string") void importPath(sel);
  };

  const remove = async (p: Project) => {
    await api.deleteProject(p.id).catch((e) => notify(errText(e), "error"));
    if (project?.id === p.id) setProject(null);
    setProjects(await api.listProjects());
  };

  return (
    <div className="flex min-h-full flex-col gap-6 pb-4">
      <div>
        <h1 className="font-display text-3xl font-extrabold tracking-tight">Закинь длинное видео</h1>
        <p className="mt-2 max-w-2xl text-white/55">
          Программа сама найдёт в нём лучшие моменты. Ты отберёшь нужные, а она соберёт из них вертикальные ролики с
          геймплеем и баннером.
        </p>
      </div>

      <div className="grid gap-6 xl:grid-cols-[1.25fr_1fr]">
        <div className="flex min-w-0 flex-col gap-3">
          <ModeTabs mode={mode} onChange={setMode} />
          <AnimatePresence mode="wait" initial={false}>
            {mode === "file" ? (
              <motion.div key="file" initial={{ opacity: 0, y: 10 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0, y: -10 }} transition={{ duration: 0.18 }}>
                <DropZone dragging={dragging} busy={busy} onPick={pick} />
              </motion.div>
            ) : (
              <motion.div key="youtube" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0, y: -10 }} transition={{ duration: 0.18 }}>
                <YoutubeCard paste={paste} />
              </motion.div>
            )}
          </AnimatePresence>
        </div>
        <AnimatePresence mode="wait">
          {project ? (
            <ProjectHero key={project.id} project={project} onNext={() => go("analysis")} />
          ) : (
            <motion.div
              key="none"
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              exit={{ opacity: 0 }}
              className="glass grid min-w-0 place-items-center rounded-[28px] p-8 text-center text-white/40"
            >
              <div>
                <Film size={40} className="mx-auto mb-3 opacity-60" />
                Здесь появится превью выбранного видео
              </div>
            </motion.div>
          )}
        </AnimatePresence>
      </div>

      <DownloadsList />

      <AssetLibrary hoverKind={hoverKind} />

      {projects.length > 0 && (
        <section>
          <h2 className="mb-3 font-display text-sm font-semibold uppercase tracking-[0.18em] text-white/45">
            Недавние
          </h2>
          <div className="grid grid-cols-[repeat(auto-fill,minmax(250px,1fr))] gap-3" style={{ perspective: 1000 }}>
            <AnimatePresence>
              {projects.map((p, i) => (
                <motion.div
                  key={p.id}
                  layout
                  initial={{ opacity: 0, rotateX: -30, y: 24 }}
                  animate={{ opacity: 1, rotateX: 0, y: 0 }}
                  exit={{ opacity: 0, scale: 0.85 }}
                  transition={{ type: "spring", stiffness: 220, damping: 22, delay: i * 0.04 }}
                  whileHover={{ y: -4, rotateX: 4, scale: 1.015 }}
                  onClick={() => setProject(p)}
                  className={`glass group relative cursor-pointer rounded-2xl p-4 ${
                    project?.id === p.id ? "!border-accent/60" : ""
                  }`}
                >
                  <div className="truncate pr-8 font-semibold">{p.title}</div>
                  <div className="mt-2 flex flex-wrap gap-1.5">
                    <span className="chip">{fmtDuration(p.duration)}</span>
                    <span className="chip">
                      {p.width}×{p.height}
                    </span>
                    {p.tight && (
                      <span className="chip !text-accent" title="Весь ролик будет нарезан подряд, без пропусков">
                        вплотную
                      </span>
                    )}
                  </div>
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      void remove(p);
                    }}
                    title="Убрать из списка (файл останется на диске)"
                    className="absolute right-3 top-3 grid h-8 w-8 place-items-center rounded-lg text-white/30 opacity-0 transition hover:bg-white/10 hover:text-danger group-hover:opacity-100"
                  >
                    <Trash size={16} />
                  </button>
                </motion.div>
              ))}
            </AnimatePresence>
          </div>
        </section>
      )}
    </div>
  );
}

function ModeTabs({ mode, onChange }: { mode: Mode; onChange: (m: Mode) => void }) {
  const tabs = [
    { id: "file" as const, label: "Файл", Icon: Upload },
    { id: "youtube" as const, label: "Ссылка YouTube", Icon: Link },
  ];
  return (
    <div className="glass-lite inline-flex self-start rounded-2xl p-1">
      {tabs.map(({ id, label, Icon }) => (
        <button
          key={id}
          onClick={() => onChange(id)}
          className={`relative z-10 flex items-center gap-2 rounded-xl px-4 py-2 text-sm font-semibold transition-colors ${
            mode === id ? "text-accent-ink" : "text-white/60 hover:text-white"
          }`}
        >
          {mode === id && (
            <motion.span
              layoutId="import-mode"
              className="absolute inset-0 -z-10 rounded-xl bg-accent"
              style={{ boxShadow: "0 6px 24px -8px var(--accent)" }}
              transition={{ type: "spring", stiffness: 380, damping: 30 }}
            />
          )}
          <Icon size={16} />
          {label}
        </button>
      ))}
    </div>
  );
}

function DropZone({ dragging, busy, onPick }: { dragging: boolean; busy: boolean; onPick: () => void }) {
  const mx = useMotionValue(0);
  const my = useMotionValue(0);
  const rx = useSpring(useTransform(my, [-0.5, 0.5], [9, -9]), { stiffness: 160, damping: 18 });
  const ry = useSpring(useTransform(mx, [-0.5, 0.5], [-11, 11]), { stiffness: 160, damping: 18 });
  const ref = useRef<HTMLDivElement>(null);

  return (
    <div style={{ perspective: 1200 }}>
      <motion.div
        ref={ref}
        onPointerMove={(e) => {
          const r = e.currentTarget.getBoundingClientRect();
          mx.set((e.clientX - r.left) / r.width - 0.5);
          my.set((e.clientY - r.top) / r.height - 0.5);
        }}
        onPointerLeave={() => {
          mx.set(0);
          my.set(0);
        }}
        animate={{ scale: dragging ? 1.035 : 1, z: dragging ? 50 : 0 }}
        transition={{ type: "spring", stiffness: 260, damping: 18 }}
        style={{ rotateX: rx, rotateY: ry, transformStyle: "preserve-3d" }}
        className="glass-spin relative flex h-[340px] flex-col items-center justify-center gap-5 rounded-[28px] p-8 text-center"
      >
        <motion.div
          animate={dragging ? { y: [0, -12, 0] } : { y: 0 }}
          transition={{ repeat: dragging ? Infinity : 0, duration: 0.9 }}
          style={{ transform: "translateZ(60px)" }}
          className="grid h-20 w-20 place-items-center rounded-3xl bg-accent text-accent-ink"
        >
          <Upload size={36} />
        </motion.div>
        <div style={{ transform: "translateZ(40px)" }}>
          <div className="font-display text-xl font-semibold">
            {dragging ? "Отпускай!" : busy ? "Читаю файл…" : "Перетащи видео сюда"}
          </div>
          <div className="mt-1.5 text-sm text-white/50">mp4 · mov · mkv · webm · любой длины</div>
        </div>
        <button className="btn" style={{ transform: "translateZ(30px)" }} onClick={onPick} disabled={busy}>
          Выбрать файл
        </button>
      </motion.div>
    </div>
  );
}

function ProjectHero({ project, onNext }: { project: Project; onNext: () => void }) {
  const [src, setSrc] = useState<string | null>(null);
  const [broken, setBroken] = useState(false);
  const setProject = useUi((s) => s.setProject);
  const setProjects = useUi((s) => s.setProjects);
  const notify = useUi((s) => s.notify);

  // Отметка «видео уже размечено» хранится в проекте и действует при следующем анализе.
  const setTight = async (tight: boolean) => {
    try {
      setProject(await api.projectSetTight(project.id, tight));
      setProjects(await api.listProjects());
      if (project.status !== "imported") notify("Пересчитай анализ на следующем шаге, чтобы отметка сработала");
    } catch (e) {
      notify(errText(e), "error");
    }
  };

  useEffect(() => {
    setBroken(false);
    api.mediaUrl(project.path).then(setSrc).catch(() => setBroken(true));
  }, [project.path]);

  return (
    <motion.div
      initial={{ opacity: 0, rotateY: 30, x: 60 }}
      animate={{ opacity: 1, rotateY: 0, x: 0 }}
      exit={{ opacity: 0, rotateY: -30, x: -60 }}
      transition={{ type: "spring", stiffness: 160, damping: 20 }}
      className="glass flex min-w-0 flex-col gap-4 rounded-[28px] p-5"
      style={{ transformOrigin: "left center" }}
    >
      <div className="relative aspect-video overflow-hidden rounded-2xl bg-black">
        {src && !broken ? (
          <video
            key={src}
            src={src}
            controls
            muted
            preload="metadata"
            className="h-full w-full object-contain"
            onError={() => setBroken(true)}
          />
        ) : (
          <div className="grid h-full place-items-center p-6 text-center text-sm text-white/50">
            {broken
              ? "Предпросмотр не открылся: окну не хватает H.264-декодера. Установи пакет gst-libav — на анализ это не влияет."
              : "Загрузка превью…"}
          </div>
        )}
      </div>
      <div>
        <div className="truncate font-display text-lg font-semibold">{project.title}</div>
        <div className="mt-2 flex flex-wrap gap-1.5">
          <span className="chip">{fmtDuration(project.duration)}</span>
          <span className="chip">
            {project.width}×{project.height}
          </span>
          <span className="chip">{Math.round(project.fps)} fps</span>
          <span className="chip">{project.has_audio ? "со звуком" : "без звука"}</span>
        </div>
      </div>
      <div className="flex items-center justify-between gap-4 rounded-2xl border border-white/10 bg-white/5 px-4 py-3">
        <div className="min-w-0">
          <div className="text-sm font-semibold">Видео уже размечено</div>
          <div className="text-xs leading-snug text-white/45">
            ИИ нарежет всё видео подряд без пропусков: вырезать ничего не нужно, границы встанут в паузах и на сменах сцены
          </div>
        </div>
        <Switch checked={project.tight} onChange={(v) => void setTight(v)} />
      </div>
      <button className="btn btn-primary self-start" onClick={onNext}>
        {project.tight ? "Нарезать видео" : "Найти моменты"} <Arrow size={18} />
      </button>
    </motion.div>
  );
}
