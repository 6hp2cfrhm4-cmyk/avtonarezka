import { useEffect, useState } from "react";
import { api, errText, fmtDuration, type AnalysisTimeline } from "../lib/api";

export default function SignalTimeline({ projectId, duration, start, end, onSelect }: { projectId: number; duration: number; start?: number; end?: number; onSelect: (at: number) => void }) {
  const [data, setData] = useState<AnalysisTimeline | null>(null);
  const [error, setError] = useState("");
  const [at, setAt] = useState(0);
  useEffect(() => { let active = true; setAt(0); setData(null); setError(""); api.analysisTimeline(projectId).then(t => active && setData(t)).catch(e => active && setError(errText(e))); return () => { active = false; }; }, [projectId]);
  const pick = (value: number) => { const t = Math.max(0, Math.min(duration - 3, value)); setAt(t); onSelect(t); };
  if (error) return <section className="factory-panel"><p role="alert">Карта сигналов: {error}</p></section>;
  if (!data) return <p className="text-sm text-white/55">Загружаю карту сигналов…</p>;
  if (!data.series.some(s => s.values)) return <section className="factory-panel"><h2>Карта сигналов</h2><p>Карта появится после анализа записи.</p></section>;
  const scout = data.scout;
  return <section className="signal-timeline" aria-label="Карта сигналов по времени">
    <div className="factory-section-heading"><h2>Карта сигналов</h2><span>{scout ? scout.full ? "Полный проход" : `Скаут: ${Math.round(scout.selectedSeconds / scout.duration * 100)}% записи` : "Сохранённый анализ"}</span></div>
    <p>Нажми на карту, чтобы задать начало своего момента. Яркость показывает всплески относительно обычного уровня записи.</p>
    <div className="signal-grid">
      <div className="signal-labels">{data.series.map(s => <span key={s.key}>{s.label}</span>)}{scout && <span>Окна скаута</span>}</div>
      <div className="min-w-0">
        <button type="button" className="signal-plot" aria-label="Выбрать время на карте сигналов" onClick={e => { const b = e.currentTarget.getBoundingClientRect(); pick((e.clientX - b.left) / b.width * duration); }}>
          <svg viewBox={`0 0 1000 ${data.series.length * 28 + (scout ? 16 : 0)}`} preserveAspectRatio="none" style={{ height: data.series.length * 28 + (scout ? 16 : 0) }} aria-hidden="true">
            {data.series.map((s, row) => <g key={s.key}>{s.values ? s.values.map((v,i) => <rect key={i} x={i * data.hop / duration * 1000} y={row * 28 + 3} width={Math.max(1, data.hop / duration * 1000 + 0.2)} height={20} fill="var(--accent)" opacity={0.08 + Math.min(1, Math.max(0, v) / 4) * 0.92} />) : <text x={10} y={row * 28 + 18} fill="#999ca3" fontSize={13}>нет сигнала</text>}</g>)}
            {scout?.windows.map((w,i) => <rect key={i} x={w.start / duration * 1000} y={data.series.length * 28 + 2} width={(w.end - w.start) / duration * 1000} height={8} fill="var(--accent)" opacity={0.5} />)}
            {start !== undefined && end !== undefined && <rect x={start / duration * 1000} y={1} width={(end - start) / duration * 1000} height={data.series.length * 28 - 1} fill="none" stroke="white" strokeWidth={1.5} />}
            <line x1={at / duration * 1000} x2={at / duration * 1000} y1={0} y2={data.series.length * 28} stroke="white" strokeWidth={1} />
          </svg>
        </button>
        <div className="signal-ticks">{[0, .25, .5, .75, 1].map(x => <span key={x}>{fmtDuration(x * duration)}</span>)}</div>
      </div>
    </div>
    <div className="signal-time-control"><label htmlFor="signal-time">Свой момент с {fmtDuration(at)}</label><input id="signal-time" aria-label="Время своего момента" className="range" type="range" min={0} max={Math.max(0, duration - 3)} step={0.5} value={at} onChange={e => pick(+e.target.value)} /></div>
    {scout && <p>{scout.speechSeconds > 0 ? `Whisper обработал ${fmtDuration(scout.speechSeconds)} из ${fmtDuration(duration)}.` : "Whisper не выполнялся: используются доступные сигналы."} Белая рамка отмечает текущий кандидат.</p>}
  </section>;
}
