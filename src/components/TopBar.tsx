import type { EnvReport } from "../lib/api";
import { installYtdlpHint, MIN_YTDLP, updateYtdlpHint } from "../lib/youtube";
import { ACCENTS, useUi } from "../store";

export default function TopBar() {
  const info = useUi((s) => s.info);
  const accent = useUi((s) => s.accent);
  const setAccent = useUi((s) => s.setAccent);
  const lite = useUi((s) => s.lite);
  const setLite = useUi((s) => s.setLite);

  return (
    <header className="flex items-center justify-between gap-4">
      <div className="flex items-center gap-3">
        <div
          className="grid h-10 w-10 place-items-center rounded-xl bg-accent text-accent-ink"
          style={{ boxShadow: "0 0 28px -4px var(--accent)" }}
        >
          <svg width="22" height="22" viewBox="0 0 24 24" fill="currentColor">
            <path d="M8 5.5v13l11-6.5z" />
          </svg>
        </div>
        <div>
          <div className="font-display text-[15px] font-extrabold tracking-wide">АВТОНАРЕЗКА</div>
          <div className="text-xs text-white/40">v{info?.version ?? "…"} · всё работает локально</div>
        </div>
      </div>

      <div className="flex items-center gap-3">
        {info && (
          <div className="flex items-center gap-1.5">
            <Status ok={!!info.env.ffmpeg} label="ffmpeg" />
            <Status ok={info.env.nvenc} label="NVENC" warn="CPU" />
            <Status ok={info.env.libass} label="субтитры" />
            <YtdlpStatus env={info.env} />
          </div>
        )}
        <div className="glass flex items-center gap-1.5 rounded-full px-2.5 py-1.5">
          {ACCENTS.map((c) => (
            <button
              key={c}
              title={c}
              onClick={() => setAccent(c)}
              className="h-4 w-4 rounded-full outline-none transition-transform hover:scale-125"
              style={{
                background: c,
                boxShadow: accent === c ? `0 0 0 2px #06060b, 0 0 0 3.5px ${c}` : "none",
              }}
            />
          ))}
        </div>
        <button
          onClick={() => setLite(!lite)}
          className={`chip cursor-pointer ${lite ? "!border-accent/50 !text-accent" : ""}`}
          title="Отключает 3D-анимацию фона и размытие — на слабых видеокартах"
        >
          {lite ? "Облегчённо" : "Полная анимация"}
        </button>
      </div>
    </header>
  );
}

/** yt-dlp для скачивания с YouTube: нет совсем — красный, старый или без Deno/Node — жёлтый. */
function YtdlpStatus({ env }: { env: EnvReport }) {
  if (!env.ytdlp) return <Status ok={false} label="yt-dlp" title={`Нужен для скачивания с YouTube: ${installYtdlpHint(env.os)}`} />;
  if (!env.ytdlpOk)
    return <Status ok={false} label={`yt-dlp ${env.ytdlp}`} warn="старая" title={`Скачивание работает, но утилита-образец требует ${MIN_YTDLP} или новее. Обновить: ${updateYtdlpHint(env.os)}`} />;
  if (!env.jsRuntime)
    return <Status ok={false} label="yt-dlp" warn="нет Deno" title={`Без Deno (или Node) YouTube может отдать неполный список качеств: ${installYtdlpHint(env.os)}`} />;
  return <Status ok label={`yt-dlp ${env.ytdlp}`} title={`JS-рантайм: ${env.jsRuntime}`} />;
}

function Status({ ok, label, warn, title }: { ok: boolean; label: string; warn?: string; title?: string }) {
  return (
    <span className="chip" title={title}>
      <span
        className="h-1.5 w-1.5 rounded-full"
        style={{ background: ok ? "var(--accent)" : warn ? "#ffb020" : "var(--danger)" }}
      />
      {ok ? label : warn ? `${label} → ${warn}` : `${label}: нет`}
    </span>
  );
}
