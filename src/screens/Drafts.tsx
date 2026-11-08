import { memo, useCallback, useEffect, useMemo, useState, type CSSProperties } from "react";
import { AnimatePresence, motion } from "motion/react";
import { api, errText, fmtDuration, type Draft, type DraftStatus } from "../lib/api";
import { useMediaUrl } from "../lib/useMediaUrl";
import { useUi } from "../store";
import { onEvent } from "../lib/events";
import { inTauri } from "../lib/api";
import { Arrow, Check } from "../components/icons";
import DraftEditor from "../components/DraftEditor";
import Tilt from "../components/Tilt";

const LABEL: Record<DraftStatus, string> = {
  queued: "В очереди",
  rendering: "Собирается",
  ready: "Готов",
  approved: "Одобрен",
  rejected: "Отклонён",
  failed: "Ошибка",
  final_queued: "Финал: в очереди",
  final_rendering: "Финал собирается",
  final_done: "Финал готов",
  final_failed: "Финал: ошибка",
};
const busyStatus = (s: DraftStatus) => s === "queued" || s === "rendering";

export default function DraftsScreen() {
  // по одному полю, а не весь стор: события рендера не должны перерисовывать сетку из сотни карточек
  const project = useUi((s) => s.project);
  const moments = useUi((s) => s.moments);
  const setMoments = useUi((s) => s.setMoments);
  const drafts = useUi((s) => s.drafts);
  const setDrafts = useUi((s) => s.setDrafts);
  const defaults = useUi((s) => s.defaults);
  const go = useUi((s) => s.go);
  const notify = useUi((s) => s.notify);
  const [editing, setEditing] = useState<number | null>(null);
  const [working, setWorking] = useState(false);

  const reload = useCallback(async () => {
    if (!project) return;
    setDrafts(await api.draftsList(project.id));
  }, [project, setDrafts]);

  useEffect(() => {
    if (!project) return;
    reload().catch((e) => notify(errText(e), "error"));
    api.momentsList(project.id).then(setMoments).catch(() => undefined);
    api.assetsList().then(useUi.getState().setAssets).catch(() => undefined);
    api.defaultsGet().then(useUi.getState().setDefaults).catch(() => undefined);
  }, [project, reload, setMoments, notify]);

  // Отладка: `curl 127.0.0.1:<порт>/dev/edit?draft=ID` открывает редактор (только debug-сборка).
  useEffect(() => {
    if (!import.meta.env.DEV || !inTauri) return;
    return onEvent<number>("dev-edit", (id) => setEditing(id));
  }, []);

  const accepted = moments.filter((m) => m.status === "accepted").length;
  const missing = Math.max(0, accepted - drafts.length);
  const noBackground = !defaults || defaults.backgroundIds.length === 0;
  const count = (st: DraftStatus[]) => drafts.filter((d) => st.includes(d.status)).length;
  const editingDraft = drafts.find((d) => d.id === editing) ?? null;

  const generate = async () => {
    if (!project) return;
    setWorking(true);
    try {
      await api.draftsGenerate(project.id);
      await reload();
    } catch (e) {
      notify(errText(e), "error");
    } finally {
      setWorking(false);
    }
  };

  const approveReady = async () => {
    if (!project) return;
    try {
      await api.draftsApproveReady(project.id);
      await reload();
    } catch (e) {
      notify(errText(e), "error");
    }
  };

  // упавшие черновики собираются заново сразу все; бэкенд подставит фон, выбранный уже после их создания
  const retryFailed = async () => {
    try {
      await Promise.all(drafts.filter((d) => d.status === "failed").map((d) => api.draftRerender(d.id)));
      await reload();
    } catch (e) {
      notify(errText(e), "error");
    }
  };

  // фон у ещё не одобренных черновиков переставляется на те же секунды, что и клип в исходном видео
  const realignGameplay = async () => {
    if (!project) return;
    try {
      const n = await api.draftsRealignGameplay(project.id);
      notify(n ? `Фон переставлен по таймкодам: роликов — ${n}, превью собираются заново` : "Фон уже стоит по таймкодам");
      await reload();
    } catch (e) {
      notify(errText(e), "error");
    }
  };

  const startFinals = async () => {
    if (!project) return;
    try {
      const n = await api.finalsStart(project.id);
      notify(`Финальный рендер запущен: роликов — ${n}`);
      go("final");
    } catch (e) {
      notify(errText(e), "error");
    }
  };

  if (!project) return <div className="grid h-full place-items-center text-white/50">Сначала выбери видео</div>;

  return (
    <div className="flex min-h-full flex-col gap-6 pb-4">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="font-display text-3xl font-extrabold tracking-tight">Проверь черновики</h1>
          <p className="mt-2 max-w-2xl text-white/55">
            Быстрые превью 540×960. Одобри нужные, неудачные отклони или поправь вручную: баннер, субтитры, фон и звук настраиваются для каждого ролика.
          </p>
        </div>
        <div className="flex flex-wrap gap-2">
          <span className="chip">Всего: {drafts.length}</span>
          <span className="chip !border-accent/40 !text-accent">Одобрено: {count(["approved"])}</span>
          <span className="chip">Собирается: {count(["queued", "rendering"])}</span>
          {count(["failed"]) > 0 && <span className="chip !border-danger/50 !text-danger">Ошибок: {count(["failed"])}</span>}
        </div>
      </div>

      {noBackground && (
        <motion.div initial={{ opacity: 0, y: -10 }} animate={{ opacity: 1, y: 0 }} className="flex items-center justify-between gap-4 rounded-2xl border border-[#ffb020]/30 bg-[#ffb020]/8 p-4 text-sm text-[#ffd88a]">
          <span>Не выбран фон для нижней половины. Добавь геймплей в библиотеке и отметь его галочкой.</span>
          <button className="btn !py-1.5 !text-xs" onClick={() => go("import")}>Открыть библиотеку</button>
        </motion.div>
      )}

      {drafts.length === 0 ? (
        <motion.div initial={{ opacity: 0, rotateX: 15, y: 30 }} animate={{ opacity: 1, rotateX: 0, y: 0 }} className="glass grid place-items-center gap-5 rounded-[28px] p-14 text-center">
          <div className="grid h-16 w-16 place-items-center rounded-2xl bg-accent font-display text-2xl font-extrabold text-accent-ink">{accepted}</div>
          <div>
            <div className="font-display text-2xl font-extrabold">{accepted ? `Собрать ${accepted} ${plural(accepted)}?` : "Нет принятых моментов"}</div>
            <p className="mt-2 text-white/55">{accepted ? "Каждый получит свой фон, баннер, субтитры и номер части." : "Вернись на шаг «Моменты» и оставь хотя бы один."}</p>
          </div>
          <button className="btn btn-primary" disabled={!accepted || working} onClick={() => void generate()}>
            {working ? "Запускаю…" : "Собрать черновики"} <Arrow size={18} />
          </button>
        </motion.div>
      ) : (
        <>
          <div className="grid grid-cols-[repeat(auto-fill,minmax(205px,1fr))] gap-6">
            {drafts.map((d, i) => (
              <DraftCard key={d.id} d={d} index={i} onEdit={setEditing} onChanged={reload} />
            ))}
          </div>

          {/* без размытия фона (glass): под панелью прокручивается вся сетка */}
          <div className="glass-lite sticky bottom-0 z-10 flex flex-wrap items-center justify-between gap-3 rounded-2xl p-4">
            <div className="flex flex-wrap gap-2">
              <button className="btn" onClick={() => void approveReady()} disabled={count(["ready"]) === 0}>
                Одобрить все готовые ({count(["ready"])})
              </button>
              {count(["failed"]) > 0 && (
                <button className="btn" onClick={() => void retryFailed()}>
                  Повторить упавшие ({count(["failed"])})
                </button>
              )}
              {count(["ready", "failed", "rejected"]) > 0 && (
                <button
                  className="btn"
                  onClick={() => void realignGameplay()}
                  title="Фон берётся с тех же секунд, что и клип в исходном видео. У не одобренных черновиков ручной выбор фона и секунды сбросится"
                >
                  Фон по таймкодам
                </button>
              )}
              {missing > 0 && (
                <button className="btn" onClick={() => void generate()} disabled={working}>
                  Добавить ещё {missing} {plural(missing)}
                </button>
              )}
            </div>
            <button className="btn btn-primary" disabled={count(["approved"]) === 0} onClick={() => void startFinals()}>
              Финальный рендер ({count(["approved"])}) <Arrow size={18} />
            </button>
          </div>
        </>
      )}

      <AnimatePresence>{editingDraft && <DraftEditor key="editor" draft={editingDraft} onClose={() => setEditing(null)} />}</AnimatePresence>
    </div>
  );
}

const plural = (n: number) => (n % 10 === 1 && n % 100 !== 11 ? "черновик" : n % 10 >= 2 && n % 10 <= 4 && (n % 100 < 10 || n % 100 >= 20) ? "черновика" : "черновиков");

/**
 * Карточка черновика. Обычный блок без `layout`, пружин и 3D-слоёв; отрисовывается только видимое (`cv-card`).
 * Без `backdrop-filter` на бейджах и оверлеях: размытие внутри скруглённой обрезки и под анимированным фоном
 * загоняло поток композитора WebKitGTK в бесконечную работу (процесс окна разрастался до гигабайтов).
 */
const DraftCard = memo(function DraftCard({
  d, index, onEdit, onChanged,
}: { d: Draft; index: number; onEdit: (id: number) => void; onChanged: () => Promise<void> }) {
  const progress = useUi((s) => s.renderProgress[d.id] ?? 0);
  const notify = useUi((s) => s.notify);
  const [hover, setHover] = useState(false);
  const poster = useMediaUrl(d.posterPath, d.updatedAt);
  const video = useMediaUrl(hover ? d.proxyPath : null, d.updatedAt);
  const busy = busyStatus(d.status);
  const done = d.status === "ready" || d.status === "approved" || d.status === "rejected" || d.status.startsWith("final");
  const accepted = d.status === "approved" || d.status === "final_done";

  const mark = async (status: DraftStatus) => {
    try {
      await api.draftSetStatus(d.id, d.status === status ? "ready" : status);
      await onChanged();
    } catch (e) {
      notify(errText(e), "error");
    }
  };

  const ring = useMemo(() => 2 * Math.PI * 26, []);
  return (
    <div className="cv-card dr-in" style={{ "--i": index } as CSSProperties} onPointerEnter={() => setHover(true)} onPointerLeave={() => setHover(false)}>
      <Tilt strength={8} className="aspect-[9/16]">
        <div
          className={`relative h-full w-full overflow-hidden rounded-[22px] border bg-ink-800 ${
            accepted ? "border-accent" : d.status === "failed" ? "border-danger/60" : "border-white/10"
          } ${d.status === "rejected" ? "opacity-45" : ""}`}
        >
          {video ? (
            <video src={video} autoPlay loop muted playsInline className="h-full w-full object-cover" />
          ) : poster ? (
            <img src={poster} alt="" className="h-full w-full object-cover" />
          ) : (
            <div className="h-full w-full bg-gradient-to-b from-ink-700 to-ink-900" />
          )}

          {/* свечение внутри карточки: внешнее обрезает cv-card */}
          {d.status === "approved" && <div className="pointer-events-none absolute inset-0 rounded-[22px]" style={{ boxShadow: "inset 0 0 28px -4px var(--accent)" }} />}

          <div className="pointer-events-none absolute inset-x-0 top-0 flex items-start justify-between p-3">
            <span className="rounded-lg bg-black/70 px-2 py-1 font-display text-xs font-semibold">Часть {d.part}</span>
            <span className={`rounded-lg px-2 py-1 text-[11px] font-bold ${accepted ? "bg-accent text-accent-ink" : d.status === "failed" ? "bg-danger text-white" : "bg-black/70"}`}>
              {LABEL[d.status]}
            </span>
          </div>

          {busy && (
            <div className="absolute inset-0 grid place-items-center bg-black/60">
              <div className="relative grid h-16 w-16 place-items-center">
                <svg width="64" height="64" className="-rotate-90">
                  <circle cx="32" cy="32" r="26" fill="none" stroke="rgba(255,255,255,0.15)" strokeWidth="5" />
                  <motion.circle cx="32" cy="32" r="26" fill="none" stroke="var(--accent)" strokeWidth="5" strokeLinecap="round" strokeDasharray={ring} animate={{ strokeDashoffset: ring * (1 - progress) }} transition={{ type: "spring", stiffness: 90, damping: 20 }} />
                </svg>
                <span className="absolute text-xs font-bold tabular-nums">{Math.round(progress * 100)}</span>
              </div>
            </div>
          )}

          {d.status === "failed" && (
            <div className="absolute inset-0 flex flex-col justify-center gap-3 bg-black/70 p-4 text-center">
              <div className="line-clamp-5 text-xs leading-snug text-[#ffd3db]">{d.error ?? "Не удалось собрать"}</div>
              <button className="btn !py-1.5 !text-xs" onClick={() => void api.draftRerender(d.id).then(onChanged)}>
                Повторить
              </button>
            </div>
          )}

          <div className="absolute inset-x-0 bottom-0 flex flex-col gap-2 bg-gradient-to-t from-black/90 via-black/60 to-transparent p-3 pt-10">
            <div>
              <div className="line-clamp-2 text-sm font-semibold leading-tight">{d.title}</div>
              <div className="mt-0.5 text-[11px] text-white/50">
                {fmtDuration(d.start)} – {fmtDuration(d.end)}
              </div>
            </div>
            <div className="flex gap-1.5">
              <button disabled={!done} onClick={() => void mark("approved")} title="Одобрить" className={`grid h-8 flex-1 place-items-center rounded-lg text-sm font-bold transition ${d.status === "approved" ? "bg-accent text-accent-ink" : "bg-white/12 hover:bg-accent hover:text-accent-ink"} disabled:opacity-30`}>
                <Check size={16} />
              </button>
              <button disabled={!done} onClick={() => void mark("rejected")} title="Отклонить" className={`grid h-8 flex-1 place-items-center rounded-lg text-sm font-bold transition ${d.status === "rejected" ? "bg-danger text-white" : "bg-white/12 hover:bg-danger"} disabled:opacity-30`}>
                ✕
              </button>
              <button onClick={() => onEdit(d.id)} title="Править вручную" className="h-8 flex-1 rounded-lg bg-white/12 text-sm font-semibold transition hover:bg-white/25">
                Править
              </button>
            </div>
          </div>
        </div>
      </Tilt>
    </div>
  );
});
