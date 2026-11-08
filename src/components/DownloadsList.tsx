import { AnimatePresence, motion } from "motion/react";
import { api, errText } from "../lib/api";
import { fmtBytes, fmtEta, fmtSpeed, PRESET_LABEL, withVersionHint } from "../lib/youtube";
import { useUi, type DownloadJob } from "../store";
import { Check, Close, Film, Stop } from "./icons";

const RING = 2 * Math.PI * 22;

/** Доля для показа. Пока идёт скачивание, не выше 99%: склейка и перенос ещё впереди. */
function shownFraction(j: DownloadJob): number | null {
  if (j.status === "done") return 1;
  if (j.stage === "merging" || j.stage === "finishing" || j.stage === "importing") return 1;
  if (j.stage === "queued") return 0;
  return j.fraction == null ? null : Math.min(j.fraction, 0.99);
}

function stageText(j: DownloadJob): string {
  if (j.status === "done") return "Добавлено в проекты";
  if (j.status === "stopped") return "Остановлено — можно продолжить";
  if (j.status === "error") return "Не получилось скачать";
  switch (j.stage) {
    case "queued":
      return "Жду очереди…";
    case "merging":
      return "Склеиваю видео и звук…";
    case "finishing":
      return "Заканчиваю…";
    case "importing":
      return "Добавляю в проекты…";
  }
  const cur = j.streams.find((s) => !s.finished);
  if (!cur) return j.streams.length ? "Скачиваю звук…" : "Подключаюсь к YouTube…";
  const what = cur.kind === "video" ? "видео" : cur.kind === "audio" ? "звук" : "видео и звук";
  return `Скачиваю ${what}${cur.label ? ` · ${cur.label}` : ""}`;
}

export default function DownloadsList() {
  const jobs = Object.values(useUi((s) => s.downloads)).reverse();
  return (
    <AnimatePresence>
      {jobs.length > 0 && (
        <motion.section key="downloads" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }}>
          <h2 className="mb-3 font-display text-sm font-semibold uppercase tracking-[0.18em] text-white/45">Загрузки</h2>
          <div className="flex flex-col gap-3">
            <AnimatePresence initial={false}>
              {jobs.map((j) => (
                <Row key={j.jobId} job={j} />
              ))}
            </AnimatePresence>
          </div>
        </motion.section>
      )}
    </AnimatePresence>
  );
}

function Row({ job }: { job: DownloadJob }) {
  const { removeDownload, setProject, notify } = useUi();
  const env = useUi((s) => s.info?.env);
  const active = job.status === "active";
  const f = shownFraction(job);

  const again = async () => {
    try {
      await api.youtubeDownload({ url: job.url, preset: job.preset, title: job.title, channel: job.channel, thumb: job.thumb });
      removeDownload(job.jobId);
    } catch (e) {
      notify(withVersionHint(errText(e), env), "error");
    }
  };

  return (
    <motion.div
      layout
      initial={{ opacity: 0, y: 18, scale: 0.97 }}
      animate={{ opacity: 1, y: 0, scale: 1 }}
      exit={{ opacity: 0, scale: 0.94, transition: { duration: 0.2 } }}
      transition={{ type: "spring", stiffness: 220, damping: 24 }}
      className={`glass-lite relative flex gap-4 rounded-2xl p-3.5 ${job.status === "error" ? "!border-danger/45" : job.status === "done" ? "!border-accent/45" : ""}`}
    >
      <div className="relative aspect-video w-[168px] shrink-0 overflow-hidden rounded-xl bg-black">
        {job.thumb ? (
          <img src={job.thumb} alt="" draggable={false} className={`h-full w-full object-cover transition-opacity ${active ? "opacity-50" : "opacity-80"}`} />
        ) : (
          <div className="grid h-full place-items-center text-white/25">
            <Film size={28} />
          </div>
        )}
        <div className="absolute inset-0 grid place-items-center bg-black/25">
          {active ? (
            <div className="relative grid h-14 w-14 place-items-center">
              <svg width="56" height="56" className="-rotate-90">
                <circle cx="28" cy="28" r="22" fill="none" stroke="rgba(255,255,255,0.18)" strokeWidth="5" />
                {f == null ? (
                  <motion.circle
                    cx="28" cy="28" r="22" fill="none" stroke="var(--accent)" strokeWidth="5" strokeLinecap="round"
                    strokeDasharray={`${RING * 0.25} ${RING}`}
                    animate={{ rotate: 360 }}
                    transition={{ repeat: Infinity, ease: "linear", duration: 1.1 }}
                    style={{ transformOrigin: "28px 28px" }}
                  />
                ) : (
                  <motion.circle
                    cx="28" cy="28" r="22" fill="none" stroke="var(--accent)" strokeWidth="5" strokeLinecap="round"
                    strokeDasharray={RING}
                    animate={{ strokeDashoffset: RING * (1 - f) }}
                    transition={{ type: "spring", stiffness: 90, damping: 20 }}
                  />
                )}
              </svg>
              {f != null && <span className="absolute text-xs font-bold tabular-nums">{Math.round(f * 100)}</span>}
            </div>
          ) : (
            <motion.div
              initial={{ scale: 0.4, opacity: 0 }}
              animate={{ scale: 1, opacity: 1 }}
              transition={{ type: "spring", stiffness: 260, damping: 16 }}
              className={`grid h-11 w-11 place-items-center rounded-full ${
                job.status === "done" ? "bg-accent text-accent-ink" : job.status === "error" ? "bg-danger text-white" : "bg-white/20 text-white"
              }`}
            >
              {job.status === "done" ? <Check size={22} /> : job.status === "error" ? <Close size={20} /> : <Stop size={18} />}
            </motion.div>
          )}
        </div>
      </div>

      <div className="flex min-w-0 flex-1 flex-col justify-between gap-2">
        <div>
          <div className="flex items-center gap-2">
            <div className="truncate font-semibold">{job.title}</div>
            <span className="chip shrink-0">{PRESET_LABEL[job.preset] ?? job.preset}</span>
          </div>
          <div className="truncate text-xs text-white/45">{job.channel}</div>
        </div>

        {job.status === "error" ? (
          <div className="select-text whitespace-pre-line text-xs leading-relaxed text-[#ffd3db]">{withVersionHint(job.error ?? "", env)}</div>
        ) : (
          <div>
            <div className="mb-1.5 flex items-baseline justify-between gap-3 text-sm">
              <span className="truncate text-white/70">{stageText(job)}</span>
              {active && f != null && <span className="tabular-nums text-accent">{Math.round(f * 100)}%</span>}
            </div>
            <div className="h-1.5 overflow-hidden rounded-full bg-white/10">
              {active && f == null ? (
                <motion.div
                  key="wait"
                  className="h-full w-1/3 rounded-full bg-accent"
                  style={{ boxShadow: "0 0 12px var(--accent)" }}
                  animate={{ x: ["-100%", "300%"] }}
                  transition={{ repeat: Infinity, duration: 1.4, ease: "easeInOut" }}
                />
              ) : (
                <motion.div
                  key="fill"
                  className={`h-full rounded-full ${job.status === "stopped" ? "bg-white/35" : "bg-accent"}`}
                  style={{ boxShadow: job.status === "stopped" ? "none" : "0 0 12px var(--accent)" }}
                  animate={{ width: `${(f ?? job.fraction ?? 0) * 100}%` }}
                  transition={{ type: "spring", stiffness: 90, damping: 20 }}
                />
              )}
            </div>
            {active && job.stage === "downloading" && (
              <div className="mt-1.5 flex flex-wrap gap-x-4 text-xs tabular-nums text-white/40">
                <span>{job.total ? `${fmtBytes(job.downloaded)} из ${fmtBytes(job.total)}` : fmtBytes(job.downloaded)}</span>
                {job.speed != null && <span>{fmtSpeed(job.speed)}</span>}
                {job.eta != null && <span>осталось {fmtEta(job.eta)}</span>}
              </div>
            )}
          </div>
        )}
      </div>

      <div className="flex shrink-0 flex-col items-stretch justify-center gap-2">
        {active && (
          <button className="btn !py-1.5 !text-xs" onClick={() => void api.youtubeCancel(job.jobId)} disabled={job.stage === "importing"}>
            <Stop size={14} /> Остановить
          </button>
        )}
        {job.status === "stopped" && (
          <button className="btn btn-primary !py-1.5 !text-xs" onClick={() => void again()}>
            Продолжить
          </button>
        )}
        {job.status === "error" && (
          <button className="btn !py-1.5 !text-xs" onClick={() => void again()}>
            Повторить
          </button>
        )}
        {job.status === "done" && job.project && (
          <button
            className="btn btn-primary !py-1.5 !text-xs"
            onClick={(e) => {
              setProject(job.project);
              (e.currentTarget.closest(".overflow-y-auto") as HTMLElement | null)?.scrollTo({ top: 0, behavior: "smooth" });
            }}
          >
            Открыть
          </button>
        )}
        {!active && (
          <button className="btn !py-1.5 !text-xs" onClick={() => removeDownload(job.jobId)} title="Убрать из списка">
            <Close size={14} /> Убрать
          </button>
        )}
      </div>
    </motion.div>
  );
}
