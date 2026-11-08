import { memo, useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, motion } from "motion/react";
import { api, errText, type JobResolve, type JobState, type PublishJob, type QueueState } from "../lib/api";
import { captionLen, parseTags } from "../lib/caption";
import { useUi } from "../store";

export const STATE_LABEL: Record<JobState, string> = {
  drafting: "Пишу описание",
  ready: "Готово к выкладке",
  queued: "В очереди",
  sending: "Выкладывается",
  done: "Выложено",
  failed: "Ошибка",
  needs_check: "Проверь в TikTok",
};

const STATE_TONE: Record<JobState, string> = {
  drafting: "border-white/15 text-white/60",
  ready: "border-accent/40 text-accent",
  queued: "border-white/25 text-white/80",
  sending: "border-accent text-accent",
  done: "border-accent bg-accent text-accent-ink",
  failed: "border-danger/60 text-danger",
  needs_check: "border-[#ffb020]/60 text-[#ffb020]",
};

/** «5:07» или «1:02:09»: сколько осталось ждать. */
export const fmtWait = (sec: number) => {
  const s = Math.max(0, Math.ceil(sec));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
};

const card = "rounded-2xl border border-white/10 bg-white/[0.04]";
const field = "w-full rounded-xl border border-white/10 bg-white/5 px-3 py-2 text-sm outline-none transition focus:border-accent/60";

/** Автовыкладка: каждому готовому ролику своё описание от модели, дальше очередь с паузой и суточным лимитом. */
export default function AutoPublishPanel() {
  const project = useUi((s) => s.project);
  const drafts = useUi((s) => s.drafts);
  const queue = useUi((s) => s.queue);
  const setQueue = useUi((s) => s.setQueue);
  const queueText = useUi((s) => s.queueText);
  const notify = useUi((s) => s.notify);
  const [confirm, setConfirm] = useState(false);
  const [applyOpen, setApplyOpen] = useState(false);
  const [listOpen, setListOpen] = useState(true);
  const [pause, setPause] = useState("");
  const [limit, setLimit] = useState("");
  const [now, setNow] = useState(() => Date.now() / 1000);

  useEffect(() => {
    if (!project) return;
    api.queueState(project.id).then(setQueue).catch((e) => notify(errText(e), "error"));
  }, [project, setQueue, notify]);

  // поля настроек берут значения при первой загрузке, дальше правит человек
  useEffect(() => {
    if (queue && pause === "" && limit === "") {
      setPause(String(queue.settings.pauseMin));
      setLimit(String(queue.settings.dailyLimit));
    }
  }, [queue, pause, limit]);

  const ticking = !!queue && (queue.nextAt !== null || queue.running);
  useEffect(() => {
    if (!ticking) return;
    const t = setInterval(() => setNow(Date.now() / 1000), 1000);
    return () => clearInterval(t);
  }, [ticking]);

  const finals = useMemo(() => drafts.filter((d) => d.status === "final_done"), [drafts]);
  const titleOf = useMemo(() => new Map(drafts.map((d) => [d.id, `Часть ${d.part} · ${d.title}`])), [drafts]);
  const jobs = queue?.jobs ?? [];
  const byDraft = useMemo(() => new Map(jobs.map((j) => [j.draftId, j])), [jobs]);
  const todo = finals.filter((d) => !byDraft.has(d.id)).length;
  const count = (st: JobState[]) => jobs.filter((j) => st.includes(j.state)).length;
  // после «Повторить» задача сразу в очереди, и запустить нужно именно её
  const startable = count(["ready", "queued"]);

  if (!project || !queue) return <div className={`${card} p-5 text-sm text-white/50`}>Загружаю очередь…</div>;

  const run = (p: Promise<unknown>) => p.catch((e) => notify(errText(e), "error"));
  const reload = () => api.queueState(project.id).then(setQueue).catch((e) => notify(errText(e), "error"));

  const saveSettings = () => {
    const p = Math.round(Number(pause));
    const l = Math.round(Number(limit));
    if (!(p >= 1) || !(l >= 1)) {
      notify("Пауза и лимит — числа не меньше 1", "error");
      setPause(String(queue.settings.pauseMin));
      setLimit(String(queue.settings.dailyLimit));
      return;
    }
    if (p === queue.settings.pauseMin && l === queue.settings.dailyLimit) return;
    void run(api.queueSettingsSet(p, l).then(reload));
  };

  const waitLeft = queue.nextAt ? queue.nextAt - now : null;

  return (
    <section className={`${card} flex flex-col gap-4 p-5`}>
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <div className="font-display text-lg font-extrabold">Автовыкладка в TikTok</div>
          <p className="mt-1 max-w-2xl text-sm text-white/55">
            Модель пишет для каждого ролика свой длинный текст и до {queue.maxTags} хэштегов ({queue.windowMin}–{queue.windowMax} знаков вместе с хэштегами), либо вы задаёте один свой текст на все ролики. Потом ролики выходят по одному через браузер.
          </p>
        </div>
        <span className={`chip ${queue.running ? "!border-accent/50 !text-accent" : ""}`}>
          {queue.generating ? "Пишу описания" : queue.running ? "Очередь идёт" : "Очередь стоит"}
        </span>
      </div>

      <div className="grid grid-cols-2 gap-3 sm:max-w-md">
        <label className="text-xs text-white/55">
          Пауза между роликами, мин
          <input className={`${field} mt-1`} inputMode="numeric" value={pause} onChange={(e) => setPause(e.target.value)} onBlur={saveSettings} />
        </label>
        <label className="text-xs text-white/55">
          Не больше за сутки
          <input className={`${field} mt-1`} inputMode="numeric" value={limit} onChange={(e) => setLimit(e.target.value)} onBlur={saveSettings} />
        </label>
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <button
          className="btn"
          disabled={queue.generating || todo === 0 || queue.running}
          onClick={() => void run(api.queuePrepare(project.id).then(reload))}
          title={todo === 0 ? "У всех готовых роликов уже есть описание" : undefined}
        >
          {queue.generating ? "Пишу описания…" : `Составить описания${todo > 0 ? ` (${todo})` : ""}`}
        </button>
        {queue.generating && <button className="btn" onClick={() => void run(api.queuePrepareCancel().then(reload))}>Остановить</button>}
        <button className="btn" disabled={queue.generating || finals.length === 0} onClick={() => setApplyOpen(true)} title="Один текст и хэштеги на все ролики, без модели">
          Свой текст на все
        </button>
        {!queue.running && (
          <button className="btn btn-primary" disabled={startable === 0 || queue.generating || !queue.chromium} onClick={() => setConfirm(true)}>
            Запустить выкладку{startable > 0 ? ` (${startable})` : ""}
          </button>
        )}
        {queue.running && (
          <>
            <button className="btn" onClick={() => void run(api.queuePause().then(reload))}>Пауза</button>
            <button className="btn" onClick={() => void run(api.queueStop(project.id).then(reload))}>Остановить и вернуть в «готово»</button>
          </>
        )}
        {!queue.chromium && <span className="text-xs text-[#ffd3db]">Нужен Chromium или Chrome: без него выкладывать нечем.</span>}
      </div>

      <div className="flex flex-wrap items-center gap-x-5 gap-y-1 text-sm text-white/65">
        <span>
          За сутки: <b className="text-white">{queue.sentToday}</b> из {queue.settings.dailyLimit}
        </span>
        <span>Выложено: {count(["done"])}</span>
        <span>В очереди: {count(["queued", "sending"])}</span>
        {count(["failed", "needs_check"]) > 0 && <span className="text-[#ffb020]">Требуют внимания: {count(["failed", "needs_check"])}</span>}
        {waitLeft !== null && queue.running && <span className="text-accent">Следующий через {fmtWait(waitLeft)}</span>}
      </div>
      {queue.note && <div className="rounded-xl border border-[#ffb020]/30 bg-[#ffb020]/8 px-4 py-3 text-sm leading-relaxed text-[#ffd88a]">{queue.note}</div>}

      {jobs.length > 0 && (
        <div>
          <button className="chip cursor-pointer" onClick={() => setListOpen(!listOpen)}>
            {listOpen ? "Свернуть описания" : `Показать описания (${jobs.length})`}
          </button>
          {listOpen && (
            <div className="mt-3 flex flex-col gap-3">
              {jobs.map((j) => (
                <JobRow key={j.id} job={j} title={titleOf.get(j.draftId) ?? `Ролик ${j.draftId}`} working={queueText[j.draftId]} q={queue} onChanged={reload} />
              ))}
            </div>
          )}
        </div>
      )}

      <AnimatePresence>
        {applyOpen && (
          <ApplyTextDialog
            queue={queue}
            // ролики, которые получат текст: без задачи, готовые или упавшие (идущие в работу и выложенные не трогаем)
            count={finals.filter((d) => { const j = byDraft.get(d.id); return !j || j.state === "ready" || j.state === "failed"; }).length}
            replaces={jobs.filter((j) => j.state === "ready" || j.state === "failed").length}
            onClose={() => setApplyOpen(false)}
            onApply={async (caption, tags) => {
              const n = await api.queueApplyText(project.id, caption, tags);
              notify(`Текст назначен роликам: ${n}`);
              await reload();
              setApplyOpen(false);
            }}
          />
        )}
      </AnimatePresence>

      <AnimatePresence>{confirm && <StartDialog queue={queue} count={startable} onClose={() => setConfirm(false)} onStart={() => run(api.queueStart(project.id, true).then(reload)).then(() => setConfirm(false))} />}</AnimatePresence>
    </section>
  );
}

const JobRow = memo(function JobRow({ job, title, working, q, onChanged }: { job: PublishJob; title: string; working?: string; q: QueueState; onChanged: () => Promise<unknown> }) {
  const notify = useUi((s) => s.notify);
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(job.caption);
  const [tags, setTags] = useState(job.hashtags.join(" "));
  const [err, setErr] = useState<string | null>(null);

  const tagList = parseTags(tags);
  const len = editing ? captionLen(text, tagList) : job.len;
  const fits = len > 0 && len <= q.captionLimit;
  const editable = job.state === "ready" || job.state === "failed";
  const run = (p: Promise<unknown>) => p.then(() => onChanged()).catch((e) => notify(errText(e), "error"));

  const save = async () => {
    try {
      await api.queueJobUpdate(job.id, text, tagList);
      setErr(null);
      setEditing(false);
      await onChanged();
    } catch (e) {
      setErr(errText(e));
    }
  };

  const resolve = (a: JobResolve) => void run(api.queueJobResolve(job.id, a));

  return (
    <div className="rounded-xl border border-white/10 bg-black/20 p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="min-w-0 truncate text-sm font-semibold">{title}</div>
        <div className="flex items-center gap-2">
          {job.state !== "drafting" && (
            <span className={`text-xs tabular-nums ${fits ? "text-accent" : "text-danger"}`} title={`Страница TikTok принимает не больше ${q.captionLimit} знаков вместе с хэштегами`}>
              {len} / {q.captionLimit} {fits ? "✓" : "✗"}
            </span>
          )}
          <span className={`rounded-lg border px-2 py-0.5 text-[11px] font-bold ${STATE_TONE[job.state]}`}>{STATE_LABEL[job.state]}</span>
        </div>
      </div>

      {job.state === "drafting" && <div className="mt-2 text-xs text-white/55">{working ?? "Жду своей очереди у модели…"}</div>}

      {job.state !== "drafting" && !editing && (
        <>
          <p className="mt-2 line-clamp-3 whitespace-pre-line text-xs leading-relaxed text-white/60">{job.caption || "Описания нет"}</p>
          <div className="mt-2 flex flex-wrap gap-1.5">
            {job.hashtags.map((t) => (
              <span key={t} className="rounded-md bg-white/8 px-2 py-0.5 text-[11px] text-white/70">{t}</span>
            ))}
          </div>
        </>
      )}

      {editing && (
        <div className="mt-3 flex flex-col gap-2">
          <textarea className={`${field} h-56 resize-y font-mono text-xs leading-relaxed`} value={text} onChange={(e) => setText(e.target.value)} />
          <label className="text-xs text-white/55">
            Хэштеги (до {q.maxTags}, через пробел)
            <input className={`${field} mt-1`} value={tags} onChange={(e) => setTags(e.target.value)} />
          </label>
          {tagList.length > q.maxTags && <div className="text-xs text-danger">Хэштегов {tagList.length}, а можно не больше {q.maxTags}</div>}
          {err && <div className="text-xs text-danger">{err}</div>}
        </div>
      )}

      {job.error && <div className="mt-2 text-xs leading-snug text-[#ffd3db]">{job.error}</div>}

      <div className="mt-3 flex flex-wrap gap-2">
        {editing ? (
          <>
            <button className="btn !py-1.5 !text-xs btn-primary" disabled={!fits || !text.trim() || tagList.length > q.maxTags || tagList.length === 0} onClick={() => void save()}>Сохранить</button>
            <button className="btn !py-1.5 !text-xs" onClick={() => { setEditing(false); setErr(null); setText(job.caption); setTags(job.hashtags.join(" ")); }}>Отмена</button>
          </>
        ) : (
          editable && <button className="btn !py-1.5 !text-xs" onClick={() => { setText(job.caption); setTags(job.hashtags.join(" ")); setEditing(true); }}>Править</button>
        )}
        {job.state === "failed" && <button className="btn !py-1.5 !text-xs" onClick={() => resolve("retry")}>Повторить</button>}
        {job.state === "needs_check" && (
          <>
            <button className="btn !py-1.5 !text-xs" onClick={() => resolve("mark_done")}>Он вышел</button>
            <button className="btn !py-1.5 !text-xs" onClick={() => resolve("retry")}>Отправить ещё раз</button>
          </>
        )}
        {(job.state === "failed" || job.state === "needs_check" || job.state === "ready") && !editing && (
          <button className="btn !py-1.5 !text-xs" onClick={() => resolve("remove")}>Убрать из очереди</button>
        )}
      </div>
    </div>
  );
});

/** Запуск очереди меняет аккаунт TikTok по-настоящему, поэтому без явного согласия ничего не начинается. */
function StartDialog({ queue, count, onClose, onStart }: { queue: QueueState; count: number; onClose: () => void; onStart: () => Promise<unknown> }) {
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const first = Math.min(count, queue.settings.dailyLimit - queue.sentToday);
  return createPortal(
    <div className="fixed inset-0 z-50 grid place-items-center p-6" style={{ perspective: 1600 }}>
      <motion.div className="absolute inset-0 bg-black/70" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} onClick={onClose} />
      <motion.div
        initial={{ opacity: 0, rotateX: 22, y: 60, scale: 0.94 }}
        animate={{ opacity: 1, rotateX: 0, y: 0, scale: 1 }}
        exit={{ opacity: 0, y: 30 }}
        transition={{ type: "spring", stiffness: 190, damping: 22 }}
        className="relative flex max-h-[90vh] w-[min(560px,100%)] flex-col gap-4 overflow-y-auto rounded-[26px] border border-white/10 bg-[#0e0e18] p-6"
      >
        <div className="font-display text-xl font-extrabold">Запустить автовыкладку?</div>
        <ul className="flex list-disc flex-col gap-2 pl-5 text-sm leading-relaxed text-white/75">
          <li>Роликов в очереди: <b className="text-white">{count}</b>. Каждый выйдет со своим описанием и хэштегами.</li>
          <li>Пауза между роликами: {queue.settings.pauseMin} мин, не больше {queue.settings.dailyLimit} за сутки. Сегодня уже выложено: {queue.sentToday}. Сразу уйдёт: {Math.max(first, 0)}, остальные позже.</li>
          <li>Программа откроет окно Chromium со страницей загрузки TikTok и <b className="text-white">сама нажмёт «Опубликовать»</b>. Видимость берётся из настроек аккаунта в TikTok Studio: проверь её заранее.</li>
          <li>Приложение должно оставаться открытым, пока идёт очередь. Если ноутбук уснёт, очередь продолжится по часам.</li>
          <li>Если TikTok попросит войти или пройти проверку (капчу), очередь встанет на паузу. Реши это сам в окне браузера и запусти очередь снова.</li>
        </ul>
        <div className="flex justify-end gap-2">
          <button className="btn" onClick={onClose}>Отмена</button>
          <button
            className="btn btn-primary"
            disabled={busy}
            onClick={() => {
              setBusy(true);
              void onStart().finally(() => setBusy(false));
            }}
          >
            {busy ? "Запускаю…" : "Запустить"}
          </button>
        </div>
      </motion.div>
    </div>,
    document.body,
  );
}

const SHARED_KEY = "avto.sharedText";

/** Один свой текст и хэштеги на все готовые ролики: модель не нужна. Прежние описания этих роликов заменяются. */
function ApplyTextDialog({ queue, count, replaces, onClose, onApply }: { queue: QueueState; count: number; replaces: number; onClose: () => void; onApply: (caption: string, tags: string[]) => Promise<unknown> }) {
  const notify = useUi((s) => s.notify);
  const saved = (() => {
    try {
      return JSON.parse(localStorage.getItem(SHARED_KEY) ?? "null") as { caption?: string; tags?: string } | null;
    } catch {
      return null;
    }
  })();
  const [caption, setCaption] = useState(saved?.caption ?? "");
  const [tags, setTags] = useState(saved?.tags ?? "");
  const [busy, setBusy] = useState(false);
  const tagList = parseTags(tags);
  const len = captionLen(caption, tagList);
  const fits = len > 0 && len <= queue.captionLimit;
  const valid = fits && caption.trim() !== "" && tagList.length >= 1 && tagList.length <= queue.maxTags;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const apply = async () => {
    setBusy(true);
    try {
      await onApply(caption, tagList);
      try {
        localStorage.setItem(SHARED_KEY, JSON.stringify({ caption, tags }));
      } catch {
        /* без сохранения текста в браузере диалог работает так же */
      }
    } catch (e) {
      notify(errText(e), "error");
    } finally {
      setBusy(false);
    }
  };

  return createPortal(
    <div className="fixed inset-0 z-50 grid place-items-center p-6" style={{ perspective: 1600 }}>
      <motion.div className="absolute inset-0 bg-black/70" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} onClick={onClose} />
      <motion.div
        initial={{ opacity: 0, rotateX: 22, y: 60, scale: 0.94 }}
        animate={{ opacity: 1, rotateX: 0, y: 0, scale: 1 }}
        exit={{ opacity: 0, y: 30 }}
        transition={{ type: "spring", stiffness: 190, damping: 22 }}
        className="relative flex max-h-[90vh] w-[min(640px,100%)] flex-col gap-4 overflow-y-auto rounded-[26px] border border-white/10 bg-[#0e0e18] p-6"
      >
        <div>
          <div className="font-display text-xl font-extrabold">Один текст на все ролики</div>
          <p className="mt-1 text-sm text-white/55">Описание и хэштеги одинаковые у всех роликов. Модель не запускается.</p>
        </div>
        <label className="block text-sm text-white/75">
          Описание
          <textarea className={`${field} mt-1.5 h-56 resize-y text-sm leading-relaxed`} value={caption} onChange={(e) => setCaption(e.target.value)} placeholder="Текст под роликом" />
        </label>
        <label className="block text-sm text-white/75">
          Хэштеги (от 1 до {queue.maxTags}, через пробел)
          <input className={`${field} mt-1.5`} value={tags} onChange={(e) => setTags(e.target.value)} placeholder="юмор сериал тикток" />
        </label>
        {tagList.length > queue.maxTags && <div className="text-xs text-danger">Хэштегов {tagList.length}, а можно не больше {queue.maxTags}</div>}
        <div className="flex flex-wrap items-center justify-between gap-2 rounded-2xl bg-black/30 px-4 py-3 text-xs text-white/60">
          <span>Получат этот текст: {count} {count === 1 ? "ролик" : "роликов"}</span>
          <span className={`tabular-nums ${fits || len === 0 ? "text-white/60" : "text-danger"}`}>{len} / {queue.captionLimit} знаков</span>
        </div>
        {replaces > 0 && <div className="rounded-xl border border-[#ffb020]/30 bg-[#ffb020]/8 px-4 py-3 text-xs leading-relaxed text-[#ffd88a]">У {replaces} из них уже есть описание: оно будет заменено этим. Ролики, которые уже стоят в очереди, выкладываются или выложены, не меняются.</div>}
        <div className="text-xs leading-relaxed text-white/45">Одинаковый длинный текст под разными роликами TikTok может счесть повтором. Если нужно разное, лучше «Составить описания».</div>
        <div className="flex justify-end gap-2">
          <button className="btn" onClick={onClose}>Отмена</button>
          <button className="btn btn-primary" disabled={!valid || busy || count === 0} onClick={() => void apply()}>{busy ? "Назначаю…" : "Назначить всем"}</button>
        </div>
      </motion.div>
    </div>,
    document.body,
  );
}
