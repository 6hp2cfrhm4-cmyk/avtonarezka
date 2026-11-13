import { useEffect, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { api, errText, fmtDuration, type AnalysisOptions, type ModelsStatus } from "../lib/api";
import { onEvent } from "../lib/events";
import { useUi } from "../store";
import { Arrow, Check } from "../components/icons";
import { Field, Switch } from "../components/Controls";

const STAGES = [
  { id: "audio", label: "Звук", hint: "Громкость по секундам" },
  { id: "events", label: "События", hint: "Смех, крики, аплодисменты" },
  { id: "speech", label: "Речь", hint: "Whisper, слова и фразы" },
  { id: "video", label: "Картинка", hint: "Динамика и смены сцен" },
  { id: "score", label: "Отбор", hint: "Кривая интереса" },
  { id: "llm", label: "Оценка ИИ", hint: "Заголовки и кульминации" },
];
const order = (id: string) => (id === "done" ? STAGES.length : STAGES.findIndex((s) => s.id === id));
const pieces = (n: number) =>
  `${n} ${n % 10 === 1 && n % 100 !== 11 ? "фрагмент" : n % 10 >= 2 && n % 10 <= 4 && (n % 100 < 10 || n % 100 >= 20) ? "фрагмента" : "фрагментов"}`;

export default function AnalysisScreen() {
  const { project, analysis, moments, go, notify } = useUi();
  const [models, setModels] = useState<ModelsStatus | null>(null);
  const [opt, setOpt] = useState<Required<Omit<AnalysisOptions, "gpuLayers">>>({
    useEvents: true,
    useSpeech: true,
    useLlm: true,
    minLen: 20,
    maxLen: 60,
    topK: 20,
  });

  useEffect(() => {
    const load = () => api.modelsStatus().then(setModels).catch((e) => notify(errText(e), "error"));
    void load();
    // модели могут докачаться уже на этом экране: тогда переключатели должны загореться без перехода
    return onEvent("models-done", () => void load());
  }, [notify]);

  if (!project) {
    return (
      <div className="grid h-full place-items-center text-white/50">Сначала выбери видео на шаге «Импорт»</div>
    );
  }

  const done = project.status !== "imported" && !analysis.running;
  const cur = analysis.running ? order(analysis.stage) : done ? STAGES.length : -1;

  const start = async () => {
    useUi.getState().setAnalysis({ running: true, stage: "audio", fraction: 0, message: "Запускаю…", notes: [] });
    try {
      await api.analysisStart(project.id, opt);
    } catch (e) {
      useUi.getState().setAnalysis({ running: false });
      notify(errText(e), "error");
    }
  };

  return (
    <div className="flex min-h-full flex-col gap-6 pb-4">
      <div>
        <h1 className="font-display text-3xl font-extrabold tracking-tight">
          {project.tight ? "Режу видео на фрагменты" : "Ищу лучшие моменты"}
        </h1>
        <p className="mt-2 max-w-2xl text-white/55">
          {project.title} · {fmtDuration(project.duration)}. Всё считается на этом компьютере, видео никуда не уходит.
          {project.tight && " Видео размечено, поэтому оно будет нарезано целиком, без пропусков между кусками."}
        </p>
      </div>

      <div className="grid gap-6 xl:grid-cols-[1.5fr_1fr]">
        <section className="glass flex flex-col gap-7 rounded-[28px] p-7">
          <div className="grid grid-cols-3 gap-4" style={{ perspective: 1100 }}>
            {STAGES.map((s, i) => {
              const state = i < cur ? "done" : i === cur ? "active" : "wait";
              return (
                <motion.div
                  key={s.id}
                  animate={
                    state === "active"
                      ? { rotateY: -9, rotateX: 4, z: 70, scale: 1.04, opacity: 1 }
                      : state === "done"
                        ? { rotateY: 0, rotateX: 0, z: 0, scale: 1, opacity: 0.95 }
                        : { rotateY: 16, rotateX: 0, z: -40, scale: 0.97, opacity: 0.5 }
                  }
                  transition={{ type: "spring", stiffness: 170, damping: 16 }}
                  style={{ transformStyle: "preserve-3d" }}
                  className={`glass-lite relative rounded-2xl p-4 ${state === "active" ? "!border-accent/60" : ""}`}
                >
                  {state === "active" && (
                    <motion.span
                      className="pointer-events-none absolute inset-0 rounded-2xl"
                      style={{ boxShadow: "0 0 40px -6px var(--accent), inset 0 0 24px -12px var(--accent)" }}
                      animate={{ opacity: [0.5, 1, 0.5] }}
                      transition={{ repeat: Infinity, duration: 1.6 }}
                    />
                  )}
                  <div
                    className={`mb-3 grid h-9 w-9 place-items-center rounded-xl text-sm font-bold ${
                      state === "done" ? "bg-accent text-accent-ink" : state === "active" ? "border border-accent text-accent" : "bg-white/8 text-white/50"
                    }`}
                  >
                    {state === "done" ? <Check size={16} /> : i + 1}
                  </div>
                  <div className="font-display text-[13px] font-semibold">{s.label}</div>
                  <div className="mt-0.5 text-xs leading-snug text-white/45">{s.hint}</div>
                </motion.div>
              );
            })}
          </div>

          <div>
            <div className="mb-2 flex items-baseline justify-between text-sm">
              <span className="text-white/70">
                {analysis.running
                  ? analysis.message
                  : done
                    ? project.tight
                      ? `Готово: видео нарезано на ${pieces(moments.length)}`
                      : `Готово: найдено моментов — ${moments.length}`
                    : "Готов к запуску"}
              </span>
              <span className="tabular-nums text-accent">{Math.round((analysis.running ? analysis.fraction : done ? 1 : 0) * 100)}%</span>
            </div>
            <div className="h-2 overflow-hidden rounded-full bg-white/10">
              <motion.div
                className="h-full rounded-full bg-accent"
                style={{ boxShadow: "0 0 16px var(--accent)" }}
                animate={{ width: `${(analysis.running ? analysis.fraction : done ? 1 : 0) * 100}%` }}
                transition={{ type: "spring", stiffness: 90, damping: 20 }}
              />
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-3">
            {analysis.running ? (
              <button className="btn" onClick={() => void api.analysisCancel(project.id)}>
                Отменить
              </button>
            ) : (
              <>
                <button className="btn btn-primary" onClick={() => void start()}>
                  {done ? "Пересчитать" : "Начать анализ"}
                </button>
                {done && (
                  <button className="btn" onClick={() => go("moments")}>
                    К моментам <Arrow size={18} />
                  </button>
                )}
              </>
            )}
          </div>

          <AnimatePresence>
            {analysis.notes.length > 0 && (
              <motion.ul
                initial={{ opacity: 0, y: 12 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0 }}
                className="flex flex-col gap-1.5 rounded-2xl border border-[#ffb020]/30 bg-[#ffb020]/8 p-4 text-sm text-[#ffd88a]"
              >
                {analysis.notes.map((n, i) => (
                  <li key={i}>• {n}</li>
                ))}
              </motion.ul>
            )}
          </AnimatePresence>
        </section>

        <section className="glass flex flex-col gap-5 rounded-[28px] p-7">
          <h2 className="font-display text-sm font-semibold uppercase tracking-[0.18em] text-white/45">Настройки</h2>

          <Toggle label="Смех, крики, аплодисменты" ok={models?.yamnet} value={opt.useEvents} onChange={(v) => setOpt({ ...opt, useEvents: v })} />
          <Toggle label="Распознавание речи" ok={models?.whisper} value={opt.useSpeech} onChange={(v) => setOpt({ ...opt, useSpeech: v })} />
          <Toggle
            label="Языковая модель"
            ok={models?.llm && models.worker}
            hint="Заголовки, подписи, лучшая секунда для баннера"
            value={opt.useLlm}
            onChange={(v) => setOpt({ ...opt, useLlm: v })}
          />

          <Field label={project.tight ? "Фрагмент не короче" : "Минимальная длина"} value={`${opt.minLen} с`}>
            <input type="range" className="range" min={10} max={60} step={5} value={opt.minLen}
              onChange={(e) => setOpt({ ...opt, minLen: +e.target.value, maxLen: Math.max(opt.maxLen, +e.target.value + 10) })} />
          </Field>
          <Field label={project.tight ? "Фрагмент не длиннее" : "Максимальная длина"} value={`${opt.maxLen} с`}>
            <input type="range" className="range" min={20} max={120} step={5} value={opt.maxLen}
              onChange={(e) => setOpt({ ...opt, maxLen: Math.max(+e.target.value, opt.minLen + 10) })} />
          </Field>
          {project.tight ? (
            <p className="text-xs leading-relaxed text-white/45">
              Сколько получится фрагментов, зависит от длины видео. Границы ИИ ставит в паузах между фразами и на сменах сцены.
            </p>
          ) : (
            <Field label="Сколько моментов искать" value={opt.topK}>
              <input type="range" className="range" min={5} max={40} step={1} value={opt.topK}
                onChange={(e) => setOpt({ ...opt, topK: +e.target.value })} />
            </Field>
          )}
        </section>
      </div>
    </div>
  );
}

function Toggle({
  label, hint, ok, value, onChange,
}: { label: string; hint?: string; ok?: boolean; value: boolean; onChange: (v: boolean) => void }) {
  const missing = ok === false;
  return (
    <div className="flex items-center justify-between gap-4">
      <div>
        <div className="text-sm font-semibold">{label}</div>
        <div className="text-xs text-white/40">{missing ? "Модель не найдена — этап будет пропущен" : hint}</div>
      </div>
      <Switch checked={value && !missing} disabled={missing} onChange={onChange} />
    </div>
  );
}
