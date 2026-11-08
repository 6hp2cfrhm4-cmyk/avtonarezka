import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { api, errText, fmtDuration, type Moment, type MomentStatus } from "../lib/api";
import { useUi } from "../store";
import { DENSE_ABOVE } from "../lib/dense";
import { Arrow, Check } from "../components/icons";

type Dir = "accept" | "reject";

const card = {
  enter: { opacity: 0, scale: 0.9, y: 60, rotateX: -18, z: -120 },
  center: { opacity: 1, scale: 1, y: 0, rotateX: 0, z: 0, x: 0, rotateY: 0, rotateZ: 0 },
  exit: (d: Dir) => ({
    opacity: 0,
    x: d === "accept" ? 720 : -720,
    rotateY: d === "accept" ? 48 : -48,
    rotateZ: d === "accept" ? 16 : -16,
    z: 160,
    transition: { duration: 0.38, ease: [0.5, 0, 0.9, 0.4] as [number, number, number, number] },
  }),
};

export default function MomentsScreen() {
  // по одному полю, а не весь стор: прогресс рендера и скачивания не должен перерисовывать колоду
  const project = useUi((s) => s.project);
  const moments = useUi((s) => s.moments);
  const setMoments = useUi((s) => s.setMoments);
  const patchMoment = useUi((s) => s.patchMoment);
  const go = useUi((s) => s.go);
  const notify = useUi((s) => s.notify);
  const [src, setSrc] = useState<string | null>(null);
  const [playing, setPlaying] = useState(true);
  const [history, setHistory] = useState<{ id: number; prev: MomentStatus }[]>([]);
  const dirRef = useRef<Dir>("accept");

  useEffect(() => {
    if (!project) return;
    api.momentsList(project.id).then(setMoments).catch((e) => notify(errText(e), "error"));
    api.mediaUrl(project.path).then(setSrc).catch(() => setSrc(null));
  }, [project, setMoments, notify]);

  const queue = useMemo(() => moments.filter((m) => m.status === "candidate"), [moments]);
  // по времени в видео: так же, как потом нумеруются части
  const accepted = useMemo(() => moments.filter((m) => m.status === "accepted").sort((a, b) => a.start - b.start), [moments]);
  const dense = accepted.length > DENSE_ABOVE;
  const giveBack = useCallback(
    (m: Moment) => {
      api.momentSetStatus(m.id, "candidate").then(() => patchMoment({ ...m, status: "candidate" })).catch((e) => notify(errText(e), "error"));
    },
    [patchMoment, notify],
  );
  const rejected = moments.filter((m) => m.status === "rejected").length;
  const top = queue[0];

  const decide = useCallback(
    async (status: Exclude<MomentStatus, "candidate">) => {
      if (!top) return;
      dirRef.current = status === "accepted" ? "accept" : "reject";
      const prev = top.status;
      patchMoment({ ...top, status });
      setHistory((h) => [...h, { id: top.id, prev }]);
      await api.momentSetStatus(top.id, status).catch((e) => notify(errText(e), "error"));
    },
    [top, patchMoment, notify],
  );

  const undo = useCallback(async () => {
    const last = history[history.length - 1];
    const m = moments.find((x) => x.id === last?.id);
    if (!last || !m) return;
    setHistory((h) => h.slice(0, -1));
    patchMoment({ ...m, status: last.prev });
    await api.momentSetStatus(m.id, last.prev).catch((e) => notify(errText(e), "error"));
  }, [history, moments, patchMoment, notify]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement)?.tagName === "INPUT") return;
      // зажатая стрелка не должна пролистывать колоду очередью вызовов
      if (e.repeat && e.key !== " ") return;
      if (e.key === "ArrowRight") void decide("accepted");
      else if (e.key === "ArrowLeft") void decide("rejected");
      else if (e.key === " ") {
        e.preventDefault();
        setPlaying((p) => !p);
      } else if (e.key === "Backspace" || e.key.toLowerCase() === "z") void undo();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [decide, undo]);

  if (!project) return <div className="grid h-full place-items-center text-white/50">Сначала выбери видео</div>;

  return (
    <div className="flex min-h-full flex-col gap-6 pb-4">
      <div className="flex items-end justify-between gap-4">
        <div>
          <h1 className="font-display text-3xl font-extrabold tracking-tight">{project.tight ? "Проверь нарезку" : "Отбери моменты"}</h1>
          <p className="mt-2 text-white/55">
            <Key>→</Key> оставить &nbsp; <Key>←</Key> пропустить &nbsp; <Key>Пробел</Key> пауза &nbsp; <Key>Z</Key> вернуть
          </p>
        </div>
        <div className="flex gap-2">
          <span className="chip">В очереди: {queue.length}</span>
          <span className="chip !border-accent/40 !text-accent">Взято: {accepted.length}</span>
          <span className="chip">Пропущено: {rejected}</span>
        </div>
      </div>

      <div className="grid gap-6 xl:grid-cols-[1.6fr_1fr]">
        <div className="relative min-h-[560px]" style={{ perspective: 1600 }}>
          {queue.slice(1, 3).map((m, i) => (
            <motion.div
              key={m.id}
              className="glass absolute inset-x-0 top-0 h-[540px] rounded-[28px]"
              animate={{ scale: 0.95 - i * 0.05, y: 22 + i * 22, z: -80 - i * 90, opacity: 0.7 - i * 0.25 }}
              transition={{ type: "spring", stiffness: 160, damping: 18 }}
            />
          ))}
          <AnimatePresence custom={dirRef.current} mode="popLayout">
            {top ? (
              <motion.div
                key={top.id}
                custom={dirRef.current}
                variants={card}
                initial="enter"
                animate="center"
                exit="exit"
                transition={{ type: "spring", stiffness: 170, damping: 17 }}
                style={{ transformStyle: "preserve-3d" }}
                className="absolute inset-x-0 top-0"
              >
                <MomentCard m={top} src={src} playing={playing} duration={project.duration} onChange={patchMoment} />
                <div className="mt-5 flex justify-center gap-4">
                  <button className="btn !px-6" onClick={() => void decide("rejected")}>
                    ← Пропустить
                  </button>
                  <button className="btn btn-primary !px-6" onClick={() => void decide("accepted")}>
                    Оставить →
                  </button>
                </div>
              </motion.div>
            ) : (
              <motion.div
                key="empty"
                initial={{ opacity: 0, scale: 0.9, rotateX: 20 }}
                animate={{ opacity: 1, scale: 1, rotateX: 0 }}
                className="glass absolute inset-x-0 top-0 grid h-[540px] place-items-center rounded-[28px] p-10 text-center"
              >
                <div>
                  <div className="mx-auto mb-4 grid h-16 w-16 place-items-center rounded-2xl bg-accent text-accent-ink">
                    <Check size={30} />
                  </div>
                  <div className="font-display text-2xl font-extrabold">
                    {moments.length ? "Все моменты просмотрены" : "Моментов пока нет"}
                  </div>
                  <p className="mt-2 text-white/55">
                    {moments.length ? `Взято: ${accepted.length}. Можно собирать черновики.` : "Запусти анализ на предыдущем шаге."}
                  </p>
                </div>
              </motion.div>
            )}
          </AnimatePresence>
        </div>

        <aside className="flex flex-col gap-4">
          <ManualAdd projectId={project.id} duration={project.duration} />
          <section className="glass flex min-h-[200px] flex-1 flex-col gap-3 rounded-[28px] p-5">
            <h2 className="font-display text-sm font-semibold uppercase tracking-[0.18em] text-white/45">Взято в работу</h2>
            <div className="flex max-h-[290px] flex-col gap-2 overflow-y-auto pr-1" style={dense ? undefined : { perspective: 800 }}>
              <AnimatePresence initial={false}>
                {accepted.map((m) => (
                  <AcceptedRow key={m.id} m={m} dense={dense} onGiveBack={giveBack} />
                ))}
              </AnimatePresence>
              {accepted.length === 0 && <div className="py-6 text-center text-sm text-white/35">Пока ничего</div>}
            </div>
            <button className="btn btn-primary mt-auto" disabled={accepted.length === 0} onClick={() => go("drafts")}>
              Собрать черновики ({accepted.length}) <Arrow size={18} />
            </button>
          </section>
        </aside>
      </div>
    </div>
  );
}

const Key = ({ children }: { children: string }) => <span className="kbd">{children}</span>;

const ROW = "flex items-center justify-between gap-3 rounded-xl border border-white/8 bg-white/5 px-3 py-2 text-left text-sm";

/** Строка списка «Взято». Когда принятых много, это обычная кнопка без анимаций и без отрисовки за экраном. */
const AcceptedRow = memo(function AcceptedRow({ m, dense, onGiveBack }: { m: Moment; dense: boolean; onGiveBack: (m: Moment) => void }) {
  const body = (
    <>
      <span className="truncate">{m.title}</span>
      <span className="shrink-0 text-xs tabular-nums text-white/40">{fmtDuration(m.end - m.start)}</span>
    </>
  );
  if (dense) {
    return (
      <button className={`${ROW} cv-row`} title="Вернуть в очередь" onClick={() => onGiveBack(m)}>
        {body}
      </button>
    );
  }
  return (
    <motion.button
      layout
      initial={{ opacity: 0, x: 40, rotateY: 30 }}
      animate={{ opacity: 1, x: 0, rotateY: 0 }}
      exit={{ opacity: 0, x: -40 }}
      whileHover={{ x: 4 }}
      onClick={() => onGiveBack(m)}
      title="Вернуть в очередь"
      className={ROW}
    >
      {body}
    </motion.button>
  );
});

function Score({ value }: { value: number }) {
  const r = 22;
  const c = 2 * Math.PI * r;
  return (
    <div className="relative grid h-14 w-14 place-items-center" title="Оценка момента">
      <svg width="56" height="56" className="-rotate-90">
        <circle cx="28" cy="28" r={r} fill="none" stroke="rgba(255,255,255,0.12)" strokeWidth="4" />
        <motion.circle
          cx="28" cy="28" r={r} fill="none" stroke="var(--accent)" strokeWidth="4" strokeLinecap="round"
          strokeDasharray={c} initial={{ strokeDashoffset: c }} animate={{ strokeDashoffset: c * (1 - value) }}
          transition={{ duration: 0.9, ease: "easeOut" }}
        />
      </svg>
      <span className="absolute font-display text-sm font-bold">{Math.round(value * 100)}</span>
    </div>
  );
}

function MomentCard({
  m, src, playing, duration, onChange,
}: { m: Moment; src: string | null; playing: boolean; duration: number; onChange: (m: Moment) => void }) {
  return (
    <div className="glass flex h-[540px] flex-col gap-4 rounded-[28px] p-5" style={{ transformStyle: "preserve-3d" }}>
      <Clip src={src} start={m.start} end={m.end} playing={playing} />
      <div className="flex items-start justify-between gap-4" style={{ transform: "translateZ(30px)" }}>
        <div className="min-w-0">
          <div className="truncate font-display text-xl font-semibold">{m.title}</div>
          <div className="mt-1.5 flex flex-wrap gap-1.5">
            <span className="chip">
              {fmtDuration(m.start)} – {fmtDuration(m.end)}
            </span>
            <span className="chip">{Math.round(m.end - m.start)} с</span>
            <span className="chip" title="Секунда, на которой верх замрёт и покажется баннер">
              баннер на {fmtDuration(m.climax - m.start)}
            </span>
            {m.manual && <span className="chip !text-accent">вручную</span>}
          </div>
        </div>
        <Score value={m.score} />
      </div>
      <p className="line-clamp-3 min-h-[3.9rem] text-sm leading-relaxed text-white/65">
        {m.transcript || "Речи в этом фрагменте нет."}
      </p>
      <Bounds m={m} duration={duration} onChange={onChange} />
    </div>
  );
}

function Clip({ src, start, end, playing }: { src: string | null; start: number; end: number; playing: boolean }) {
  const ref = useRef<HTMLVideoElement>(null);
  const [broken, setBroken] = useState(false);

  useEffect(() => {
    const v = ref.current;
    if (!v) return;
    const seek = () => (v.currentTime = start);
    if (v.readyState >= 1) seek();
    else v.addEventListener("loadedmetadata", seek, { once: true });
  }, [start, src]);

  useEffect(() => {
    const v = ref.current;
    if (!v) return;
    if (playing) void v.play().catch(() => undefined);
    else v.pause();
  }, [playing, src, start]);

  return (
    <div className="relative h-[230px] overflow-hidden rounded-2xl bg-black" style={{ transform: "translateZ(10px)" }}>
      {src && !broken ? (
        <video
          ref={ref}
          src={src}
          muted
          autoPlay
          playsInline
          preload="auto"
          className="h-full w-full object-contain"
          onTimeUpdate={(e) => {
            const v = e.currentTarget;
            if (v.currentTime >= end || v.currentTime < start - 1) v.currentTime = start;
          }}
          onError={() => setBroken(true)}
        />
      ) : (
        <div className="grid h-full place-items-center p-6 text-center text-sm text-white/45">
          {broken
            ? "Видео не воспроизводится: окну нужен декодер H.264 (пакет gst-libav). Отбирать можно и по тексту."
            : "Загрузка видео…"}
        </div>
      )}
    </div>
  );
}

/** Три ползунка: начало, конец и секунда баннера. Сохраняется при отпускании. */
function Bounds({ m, duration, onChange }: { m: Moment; duration: number; onChange: (m: Moment) => void }) {
  const notify = useUi((s) => s.notify);
  const [v, setV] = useState({ start: m.start, end: m.end, climax: m.climax });
  const lo = Math.max(0, m.start - 30);
  const hi = Math.min(duration, m.end + 30);

  useEffect(() => setV({ start: m.start, end: m.end, climax: m.climax }), [m.id, m.start, m.end, m.climax]);

  const commit = async () => {
    if (v.start === m.start && v.end === m.end && v.climax === m.climax) return;
    try {
      onChange(await api.momentUpdate(m.id, v.start, v.end, v.climax));
    } catch (e) {
      notify(errText(e), "error");
      setV({ start: m.start, end: m.end, climax: m.climax });
    }
  };
  const row = (label: string, key: "start" | "end" | "climax", min: number, max: number) => (
    <label className="grid grid-cols-[64px_1fr_52px] items-center gap-3 text-xs text-white/50">
      {label}
      <input
        type="range" className="range" min={min} max={Math.max(max, min + 1)} step={0.5} value={v[key]}
        onChange={(e) => setV({ ...v, [key]: +e.target.value })}
        onPointerUp={() => void commit()} onKeyUp={() => void commit()}
      />
      <span className="text-right tabular-nums text-white/70">{fmtDuration(v[key])}</span>
    </label>
  );
  return (
    <div className="mt-auto flex flex-col gap-2.5" style={{ transform: "translateZ(20px)" }}>
      {row("Начало", "start", lo, v.end - 3)}
      {row("Конец", "end", v.start + 3, hi)}
      {row("Баннер", "climax", v.start + 0.2 * (v.end - v.start), v.end - 0.1 * (v.end - v.start))}
    </div>
  );
}

function parseTime(t: string): number | null {
  const parts = t.trim().split(":").map(Number);
  if (parts.some((p) => !Number.isFinite(p)) || parts.length > 3 || parts.length === 0) return null;
  return parts.reduce((a, p) => a * 60 + p, 0);
}

function ManualAdd({ projectId, duration }: { projectId: number; duration: number }) {
  const notify = useUi((s) => s.notify);
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");

  const add = async () => {
    const [a, b] = [parseTime(from), parseTime(to)];
    if (a === null || b === null || b <= a) return notify("Укажи начало и конец, например 12:30 и 13:10", "error");
    try {
      const m = await api.momentAddManual(projectId, a, Math.min(b, duration));
      // новый момент дописываем в стор, а не перечитываем весь список: при сотне моментов это O(N²) строк через IPC
      const { moments, setMoments } = useUi.getState();
      setMoments([...moments, m]);
      setFrom("");
      setTo("");
    } catch (e) {
      notify(errText(e), "error");
    }
  };
  const input = "w-full rounded-xl border border-white/10 bg-white/5 px-3 py-2 text-sm outline-none focus:border-accent/60";
  return (
    <section className="glass rounded-[28px] p-5">
      <h2 className="mb-3 font-display text-sm font-semibold uppercase tracking-[0.18em] text-white/45">Свой момент</h2>
      <div className="grid grid-cols-[1fr_1fr_auto] gap-2">
        <input className={input} placeholder="с 12:30" value={from} onChange={(e) => setFrom(e.target.value)} onKeyDown={(e) => e.key === "Enter" && void add()} />
        <input className={input} placeholder="до 13:10" value={to} onChange={(e) => setTo(e.target.value)} onKeyDown={(e) => e.key === "Enter" && void add()} />
        <button className="btn" onClick={() => void add()}>
          Добавить
        </button>
      </div>
    </section>
  );
}
