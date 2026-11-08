import { useCallback, useEffect, useRef, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { api, errText, fmtDuration, type YoutubeInfo } from "../lib/api";
import { extractYoutubeLink, fmtBytes, installYtdlpHint, resLabel, withVersionHint } from "../lib/youtube";
import { useUi } from "../store";
import { Download, Film, Link } from "./icons";

const input =
  "select-text w-full rounded-xl border border-white/10 bg-white/5 px-3 py-2.5 text-sm outline-none transition focus:border-accent/60";

type Phase = "idle" | "checking" | "ready" | "error";

/** 1080p, если есть: хватает для вертикальных 1080×1920 и качается быстрее. Иначе лучшее из доступного. */
function defaultPreset(i: YoutubeInfo): string {
  const ok = (k: string) => i.presets.some((p) => p.key === k && p.available);
  return ok("1080") ? "1080" : ok("720") ? "720" : "best";
}

/**
 * Карточка «Скачать с YouTube»: ссылка → превью и качество → «Скачать».
 * Дальше ход загрузки показывает блок «Загрузки» (DownloadsList), а эта карточка снова готова к новой ссылке.
 * `paste` приходит с экрана, когда ссылку вставили через Ctrl+V вне поля.
 */
export default function YoutubeCard({ paste }: { paste: { url: string; n: number } | null }) {
  const notify = useUi((s) => s.notify);
  const setAppInfo = useUi((s) => s.setInfo);
  const env = useUi((s) => s.info?.env);
  const [text, setText] = useState("");
  const [phase, setPhase] = useState<Phase>("idle");
  const [info, setInfo] = useState<YoutubeInfo | null>(null);
  const [preset, setPreset] = useState("1080");
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [dir, setDir] = useState("");
  // ответ на устаревший запрос (вставили другую ссылку) отбрасывается
  const seq = useRef(0);
  const field = useRef<HTMLInputElement>(null);

  useEffect(() => {
    api.outputDirGet().then(setDir).catch(() => {});
  }, []);

  const check = useCallback(
    async (raw: string) => {
      const value = raw.trim();
      if (!value) return;
      const mine = ++seq.current;
      setPhase("checking");
      setError(null);
      setInfo(null);
      try {
        const i = await api.youtubeAnalyze(value);
        if (mine !== seq.current) return;
        setInfo(i);
        setPreset(defaultPreset(i));
        setPhase("ready");
      } catch (e) {
        if (mine !== seq.current) return;
        setError(errText(e));
        setPhase("error");
      }
    },
    [],
  );

  useEffect(() => {
    if (!paste) return;
    setText(paste.url);
    void check(paste.url);
  }, [paste, check]);

  const reset = () => {
    seq.current++;
    setText("");
    setInfo(null);
    setError(null);
    setPhase("idle");
    field.current?.focus();
  };

  const start = async () => {
    if (!info) return;
    setStarting(true);
    try {
      await api.youtubeDownload({ url: info.url, preset, title: info.title, channel: info.channel, thumb: info.thumb });
      reset();
    } catch (e) {
      notify(withVersionHint(errText(e), env), "error");
    } finally {
      setStarting(false);
    }
  };

  const recheckTools = () => api.appInfo().then(setAppInfo).catch((e) => notify(errText(e), "error"));
  const selected = info?.presets.find((p) => p.key === preset);
  // Когда yt-dlp нет, об этом уже сказано в красной плашке: вторую такую же ошибку не показываем
  const view: Phase = phase === "error" && env && !env.ytdlp ? "idle" : phase;

  return (
    <motion.div
      initial={{ opacity: 0, rotateX: -14, y: 18 }}
      animate={{ opacity: 1, rotateX: 0, y: 0 }}
      transition={{ type: "spring", stiffness: 190, damping: 22 }}
      className="glass-spin relative flex min-h-[340px] flex-col gap-4 rounded-[28px] p-6"
    >
      <div className="flex items-center gap-2">
        <div className="relative flex-1">
          <Link size={17} className="pointer-events-none absolute left-3.5 top-1/2 -translate-y-1/2 text-white/40" />
          <input
            ref={field}
            className={`${input} !pl-10`}
            placeholder="Ссылка на видео YouTube"
            spellCheck={false}
            autoComplete="off"
            value={text}
            onChange={(e) => {
              setText(e.target.value);
              if (phase !== "idle") {
                seq.current++;
                setPhase("idle");
                setInfo(null);
                setError(null);
              }
            }}
            onPaste={(e) => {
              // вставили ссылку (даже внутри текста): сразу проверяем, Enter не нужен
              const link = extractYoutubeLink(e.clipboardData.getData("text"));
              if (link) {
                e.preventDefault();
                setText(link);
                void check(link);
              }
            }}
            onKeyDown={(e) => e.key === "Enter" && void check(text)}
          />
        </div>
        <button className="btn" disabled={!text.trim() || phase === "checking"} onClick={() => void check(text)}>
          Найти
        </button>
      </div>

      {env && !env.ytdlp && (
        <div className="flex items-start justify-between gap-3 rounded-xl border border-danger/40 bg-danger/10 p-3 text-xs leading-relaxed text-[#ffd3db]">
          <span>
            yt-dlp не найден. Установи его вместе с Deno: <span className="select-text font-semibold">{installYtdlpHint(env.os)}</span>
          </span>
          <button className="btn shrink-0 !py-1 !text-xs" onClick={() => void recheckTools()}>
            Проверить снова
          </button>
        </div>
      )}

      <div className="flex flex-1 flex-col justify-center">
        <AnimatePresence mode="wait" initial={false}>
          {view === "idle" && (
            <motion.div key="idle" initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0, y: -8 }} transition={{ duration: 0.15 }} className="text-center">
              <div
                className="mx-auto mb-4 grid h-16 w-16 place-items-center rounded-3xl bg-accent text-accent-ink"
                style={{ boxShadow: "0 0 34px -6px var(--accent)" }}
              >
                <Download size={30} />
              </div>
              <div className="font-display text-lg font-semibold">Вставь ссылку — видео скачается само</div>
              <div className="mt-1.5 text-sm text-white/50">
                Можно просто нажать <span className="kbd">Ctrl</span> <span className="kbd">V</span> на этом экране
              </div>
            </motion.div>
          )}

          {view === "checking" && (
            <motion.div key="checking" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} transition={{ duration: 0.15 }} className="flex flex-col gap-3">
              <div className="flex gap-4">
                <div className="yt-shimmer aspect-video w-[176px] shrink-0 rounded-xl" />
                <div className="flex-1 space-y-3 pt-1">
                  <div className="yt-shimmer h-4 w-4/5 rounded-md" />
                  <div className="yt-shimmer h-3 w-1/3 rounded-md" />
                  <div className="flex gap-1.5 pt-1">
                    <div className="yt-shimmer h-6 w-16 rounded-full" />
                    <div className="yt-shimmer h-6 w-14 rounded-full" />
                    <div className="yt-shimmer h-6 w-16 rounded-full" />
                  </div>
                </div>
              </div>
              <div className="text-sm text-white/50">Читаю сведения о видео…</div>
            </motion.div>
          )}

          {view === "error" && (
            <motion.div key="error" initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0 }} transition={{ duration: 0.15 }} className="flex flex-col items-start gap-3">
              <div className="select-text whitespace-pre-line rounded-xl border border-danger/40 bg-danger/10 p-3.5 text-sm leading-relaxed text-[#ffd3db]">
                {withVersionHint(error ?? "Не получилось прочитать видео", env)}
              </div>
              <button className="btn !py-1.5 !text-xs" onClick={() => void check(text)}>
                Попробовать ещё раз
              </button>
            </motion.div>
          )}

          {view === "ready" && info && (
            <motion.div key="ready" initial={{ opacity: 0, y: 10 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0, y: -8 }} transition={{ duration: 0.15 }} className="flex flex-col gap-4">
              <div className="flex gap-4">
                <div className="relative aspect-video w-[176px] shrink-0 overflow-hidden rounded-xl bg-black">
                  {info.thumb ? (
                    <img src={info.thumb} alt="" className="h-full w-full object-cover" draggable={false} />
                  ) : (
                    <div className="grid h-full place-items-center text-white/30">
                      <Film size={30} />
                    </div>
                  )}
                  {info.duration != null && (
                    <span className="absolute bottom-1.5 right-1.5 rounded-md bg-black/75 px-1.5 py-0.5 text-[11px] font-bold tabular-nums">
                      {fmtDuration(info.duration)}
                    </span>
                  )}
                </div>
                <div className="min-w-0 flex-1">
                  <div className="line-clamp-2 font-display text-[15px] font-semibold leading-snug">{info.title}</div>
                  <div className="mt-1 truncate text-sm text-white/50">{info.channel}</div>
                  <div className="mt-2.5 flex flex-wrap gap-1.5">
                    {info.maxHeight > 0 && <span className="chip">до {resLabel(info.maxHeight)}</span>}
                    {info.bestFps != null && <span className="chip">{Math.round(info.bestFps)} fps</span>}
                    {info.bestCodec && <span className="chip">{info.bestCodec}</span>}
                    {info.hdr && <span className="chip !border-accent/40 !text-accent">HDR</span>}
                  </div>
                </div>
              </div>

              <div>
                <div
                  className="grid rounded-xl border border-white/10 bg-white/5 p-1"
                  style={{ gridTemplateColumns: `repeat(${info.presets.length}, 1fr)` }}
                >
                  {info.presets.map((p) => (
                    <button
                      key={p.key}
                      disabled={!p.available}
                      onClick={() => setPreset(p.key)}
                      title={p.available ? undefined : "У этого видео нет такого качества"}
                      className={`rounded-lg px-2 py-1.5 text-xs font-semibold transition-colors ${
                        preset === p.key ? "bg-accent text-accent-ink" : "text-white/60 hover:text-white"
                      } disabled:cursor-not-allowed disabled:text-white/20 disabled:hover:text-white/20`}
                    >
                      {p.label}
                    </button>
                  ))}
                </div>
                <div className="mt-1.5 flex justify-between text-xs text-white/40">
                  <span className="shrink-0 whitespace-nowrap">{selected?.approxBytes ? `≈ ${fmtBytes(selected.approxBytes)}` : "Размер заранее неизвестен"}</span>
                  {dir && <span className="min-w-0 truncate pl-3">Сохранится в {dir}/Скачанное</span>}
                </div>
              </div>

              <div className="flex items-center gap-3">
                <button className="btn btn-primary" disabled={starting || !selected?.available} onClick={() => void start()}>
                  <Download size={18} /> Скачать
                </button>
                <button className="btn" onClick={reset}>
                  Другая ссылка
                </button>
              </div>
            </motion.div>
          )}
        </AnimatePresence>
      </div>
    </motion.div>
  );
}
