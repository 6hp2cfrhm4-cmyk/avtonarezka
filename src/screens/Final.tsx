import { memo, useCallback, useEffect, useMemo, useState, type CSSProperties } from "react";
import { AnimatePresence, motion } from "motion/react";
import { open } from "@tauri-apps/plugin-dialog";
import { api, errText, fmtDuration, inTauri, type Draft, type DraftStatus } from "../lib/api";
import { useMediaUrl } from "../lib/useMediaUrl";
import { useUi } from "../store";
import { Arrow, Check } from "../components/icons";
import Tilt from "../components/Tilt";
import TikTokPanel from "../components/TikTokPanel";
import PublishDialog from "../components/PublishDialog";
import AutoPublishPanel, { STATE_LABEL as JOB_LABEL } from "../components/AutoPublishPanel";

const FINAL: DraftStatus[] = ["approved", "final_queued", "final_rendering", "final_done", "final_failed"];
const LABEL: Partial<Record<DraftStatus, string>> = {
  approved: "Ждёт запуска",
  final_queued: "В очереди",
  final_rendering: "Собирается",
  final_done: "Готов",
  final_failed: "Ошибка",
};

export default function FinalScreen() {
  // по одному полю, а не весь стор: прогресс рендера не должен перерисовывать сетку
  const project = useUi((s) => s.project);
  const drafts = useUi((s) => s.drafts);
  const setDrafts = useUi((s) => s.setDrafts);
  const setPosts = useUi((s) => s.setPosts);
  const go = useUi((s) => s.go);
  const notify = useUi((s) => s.notify);
  const [outDir, setOutDir] = useState("");
  const [showTikTok, setShowTikTok] = useState(false);
  const [publishing, setPublishing] = useState<number | null>(null);

  const reload = useCallback(async () => {
    if (!project) return;
    setDrafts(await api.draftsList(project.id));
    setPosts(await api.postsList(project.id));
  }, [project, setDrafts, setPosts]);

  useEffect(() => {
    reload().catch((e) => notify(errText(e), "error"));
    api.outputDirGet().then(setOutDir).catch(() => undefined);
  }, [reload, notify]);

  const finals = useMemo(() => drafts.filter((d) => FINAL.includes(d.status)), [drafts]);
  const publishingDraft = drafts.find((d) => d.id === publishing) ?? null;
  const count = (st: DraftStatus[]) => finals.filter((d) => st.includes(d.status)).length;
  const startable = count(["approved", "final_failed"]);

  const start = async () => {
    if (!project) return;
    try {
      await api.finalsStart(project.id);
      await reload();
    } catch (e) {
      notify(errText(e), "error");
    }
  };

  const changeDir = async () => {
    if (!inTauri) return;
    const sel = await open({ directory: true, multiple: false, defaultPath: outDir || undefined });
    if (typeof sel !== "string") return;
    try {
      await api.outputDirSet(sel);
      setOutDir(sel);
    } catch (e) {
      notify(errText(e), "error");
    }
  };

  if (!project) return <div className="grid h-full place-items-center text-white/50">Сначала выбери видео</div>;

  return (
    <div className="flex min-h-full flex-col gap-6 pb-4">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="font-display text-3xl font-extrabold tracking-tight">Готовые ролики</h1>
          <p className="mt-2 max-w-2xl text-white/55">Финальный рендер 1080×1920 с нормализацией громкости. Файлы лежат в папке ниже.</p>
        </div>
        <div className="flex flex-wrap gap-2">
          <span className="chip !border-accent/40 !text-accent">Готово: {count(["final_done"])}</span>
          <span className="chip">В работе: {count(["final_queued", "final_rendering"])}</span>
          {count(["final_failed"]) > 0 && <span className="chip !border-danger/50 !text-danger">Ошибок: {count(["final_failed"])}</span>}
        </div>
      </div>

      <section className="glass flex flex-wrap items-center gap-3 rounded-2xl p-4">
        <div className="min-w-0 flex-1">
          <div className="text-xs uppercase tracking-[0.18em] text-white/40">Папка с роликами</div>
          <div className="mt-1 truncate font-mono text-sm text-white/80" title={outDir}>{outDir || "…"}</div>
        </div>
        <button className="btn" onClick={() => void changeDir()}>Изменить</button>
        <button className="btn" onClick={() => void api.outputDirOpen().catch((e) => notify(errText(e), "error"))}>Открыть папку</button>
      </section>

      <div>
        <button className="chip cursor-pointer" onClick={() => setShowTikTok(!showTikTok)}>
          {showTikTok ? "Скрыть настройки TikTok" : "Настройки TikTok"}
        </button>
        <AnimatePresence initial={false}>
          {showTikTok && (
            <motion.div initial={{ height: 0, opacity: 0, rotateX: -12 }} animate={{ height: "auto", opacity: 1, rotateX: 0 }} exit={{ height: 0, opacity: 0 }} className="overflow-hidden pt-3" style={{ transformOrigin: "top" }}>
              <TikTokPanel />
            </motion.div>
          )}
        </AnimatePresence>
      </div>

      {count(["final_done"]) > 0 && <AutoPublishPanel />}

      {finals.length === 0 ? (
        <motion.div initial={{ opacity: 0, rotateX: 15, y: 30 }} animate={{ opacity: 1, rotateX: 0, y: 0 }} className="glass grid place-items-center gap-5 rounded-[28px] p-14 text-center">
          <div className="grid h-16 w-16 place-items-center rounded-2xl bg-white/10 text-white/60"><Check size={30} /></div>
          <div>
            <div className="font-display text-2xl font-extrabold">Пока нечего собирать</div>
            <p className="mt-2 text-white/55">Одобри черновики на шаге «Черновики», и они появятся здесь.</p>
          </div>
          <button className="btn" onClick={() => go("drafts")}>К черновикам <Arrow size={18} /></button>
        </motion.div>
      ) : (
        <>
          {startable > 0 && (
            <div className="flex items-center justify-between gap-4 rounded-2xl border border-accent/30 bg-accent/8 p-4">
              <span className="text-sm text-white/80">Ждут финального рендера: {startable}</span>
              <button className="btn btn-primary" onClick={() => void start()}>Собрать финал ({startable}) <Arrow size={18} /></button>
            </div>
          )}
          <div className="grid grid-cols-[repeat(auto-fill,minmax(215px,1fr))] gap-6">
            {finals.map((d, i) => (
              <FinalCard key={d.id} d={d} index={i} onChanged={reload} onPublish={setPublishing} />
            ))}
          </div>
        </>
      )}

      <AnimatePresence>
        {publishingDraft && <PublishDialog key="publish" draft={publishingDraft} onClose={() => setPublishing(null)} onOpenSettings={() => setShowTikTok(true)} />}
      </AnimatePresence>
    </div>
  );
}

/** Карточка готового ролика: плоская, без `backdrop-filter` и 3D-слоёв (см. DraftCard в Drafts.tsx). */
const FinalCard = memo(function FinalCard({
  d, index, onChanged, onPublish,
}: { d: Draft; index: number; onChanged: () => Promise<void>; onPublish: (id: number) => void }) {
  const progress = useUi((s) => s.renderProgress[d.id] ?? 0);
  const notify = useUi((s) => s.notify);
  const post = useUi((s) => s.posts[d.id]);
  const sending = useUi((s) => s.postProgress[d.id]);
  const job = useUi((s) => s.queueJobs[d.id]);
  const poster = useMediaUrl(d.posterPath, d.updatedAt);
  const busy = d.status === "final_queued" || d.status === "final_rendering";
  const done = d.status === "final_done";
  const failed = d.status === "final_failed";
  const run = (p: Promise<unknown>) => p.catch((e) => notify(errText(e), "error"));

  return (
    <div className="cv-card dr-in" style={{ "--i": index } as CSSProperties}>
      <Tilt strength={7} className="aspect-[9/16]">
        <div className={`relative h-full w-full overflow-hidden rounded-[22px] border bg-ink-800 ${done ? "border-accent" : failed ? "border-danger/60" : "border-white/10"}`}>
          {poster ? <img src={poster} alt="" className="h-full w-full object-cover" /> : <div className="h-full w-full bg-gradient-to-b from-ink-700 to-ink-900" />}

          {/* свечение внутри карточки: внешнее обрезает cv-card */}
          {done && <div className="pointer-events-none absolute inset-0 rounded-[22px]" style={{ boxShadow: "inset 0 0 28px -4px var(--accent)" }} />}

          <div className="pointer-events-none absolute inset-x-0 top-0 flex items-start justify-between p-3">
            <span className="rounded-lg bg-black/70 px-2 py-1 font-display text-xs font-semibold">Часть {d.part}</span>
            <span className={`rounded-lg px-2 py-1 text-[11px] font-bold ${done ? "bg-accent text-accent-ink" : failed ? "bg-danger text-white" : "bg-black/70"}`}>
              {LABEL[d.status]}
            </span>
          </div>

          {busy && (
            <div className="absolute inset-x-4 top-1/2 -translate-y-1/2">
              <div className="mb-2 text-center text-xs tabular-nums text-white/80">
                {d.status === "final_queued" ? "Ждёт своей очереди" : `${Math.round(progress * 100)}%`}
              </div>
              <div className="h-2 overflow-hidden rounded-full bg-white/15">
                <motion.div className="h-full rounded-full bg-accent" style={{ boxShadow: "0 0 12px var(--accent)" }} animate={{ width: `${progress * 100}%` }} transition={{ type: "spring", stiffness: 100, damping: 20 }} />
              </div>
            </div>
          )}

          {failed && (
            <div className="absolute inset-0 flex flex-col justify-center gap-3 bg-black/75 p-4 text-center">
              <div className="line-clamp-6 text-xs leading-snug text-[#ffd3db]">{d.error ?? "Не удалось собрать"}</div>
            </div>
          )}

          <div className="absolute inset-x-0 bottom-0 flex flex-col gap-2 bg-gradient-to-t from-black/90 via-black/60 to-transparent p-3 pt-10">
            <div>
              <div className="line-clamp-2 text-sm font-semibold leading-tight">{d.title}</div>
              <div className="mt-0.5 text-[11px] text-white/50">{fmtDuration(d.end - d.start)}</div>
            </div>
            {done && (post || sending) && (
              <div className="text-[11px] leading-snug" title={post?.error ?? post?.detail ?? sending?.message}>
                {sending ? (
                  <>
                    <div className="mb-1 line-clamp-1 text-white/70">{sending.message}</div>
                    <div className="h-1.5 overflow-hidden rounded-full bg-white/15">
                      <motion.div className="h-full rounded-full bg-accent" animate={{ width: `${sending.fraction * 100}%` }} transition={{ type: "spring", stiffness: 100, damping: 20 }} />
                    </div>
                  </>
                ) : post?.status === "done" ? (
                  <span className="line-clamp-2 text-accent">TikTok: {post.detail}</span>
                ) : post?.status === "failed" ? (
                  <span className="line-clamp-2 text-[#ff8da1]">TikTok: {post.error}</span>
                ) : null}
              </div>
            )}
            {done && job && job.state !== "done" && (
              <div className={`text-[11px] leading-snug ${job.state === "failed" || job.state === "needs_check" ? "text-[#ffb020]" : "text-white/70"}`} title={job.error ?? undefined}>
                Автовыкладка: {JOB_LABEL[job.state]}
              </div>
            )}
            <div className="flex gap-1.5">
              {done && (
                <>
                  <button className="h-8 flex-1 rounded-lg bg-accent text-sm font-semibold text-accent-ink" onClick={() => void run(api.finalOpen(d.id, false))}>Открыть</button>
                  <button className="h-8 flex-1 rounded-lg bg-white/12 text-sm font-semibold hover:bg-white/25" onClick={() => void run(api.finalOpen(d.id, true))}>В папке</button>
                  <button
                    className="h-8 flex-1 rounded-lg border border-accent/50 text-sm font-semibold text-accent hover:bg-accent/15 disabled:opacity-40"
                    disabled={!!sending}
                    onClick={() => onPublish(d.id)}
                    title="Отправить в TikTok"
                  >
                    TikTok
                  </button>
                </>
              )}
              {busy && (
                <button className="h-8 flex-1 rounded-lg bg-white/12 text-sm font-semibold hover:bg-white/25" onClick={() => void run(api.draftCancel(d.id))}>Отменить</button>
              )}
              {failed && (
                <button
                  className="h-8 flex-1 rounded-lg bg-white/12 text-sm font-semibold hover:bg-white/25"
                  onClick={() => void run(api.draftSetStatus(d.id, "approved").then(() => api.finalsStart(d.projectId)).then(onChanged))}
                >
                  Повторить
                </button>
              )}
              {d.status === "approved" && <div className="grid h-8 flex-1 place-items-center rounded-lg bg-white/8 text-xs text-white/50">Ждёт запуска</div>}
            </div>
          </div>
        </div>
      </Tilt>
    </div>
  );
});
