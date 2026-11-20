import { useCallback, useEffect, useRef, useState } from "react";
import { api, errText, type LiveSession, type LiveSignal } from "../lib/api";
import { FactoryHeading, Field, LoadError, useFactoryData } from "../components/FactoryControls";
import { onEvent } from "../lib/events";

const STATE: Record<LiveSession["state"], string> = { starting: "подключаюсь", live: "идёт", ended: "эфир закончился", stopped: "остановлено", failed: "ошибка" };
const PLATFORM: Record<string, string> = { twitch: "Twitch", kick: "Kick", youtube: "YouTube" };
const KEEP = 120;
const dur = (s: number) => { const m = Math.floor(s / 60); return m >= 60 ? `${Math.floor(m / 60)} ч ${m % 60} мин` : `${m} мин`; };

/** Лента сигналов: отклонение от обычного за последние замеры; пунктир — порог момента. */
function Spark({ points }: { points: LiveSignal[] }) {
  const W = 300, H = 56, top = 12;
  const path = points.map((p, i) => `${i ? "L" : "M"}${(i / (KEEP - 1)) * W},${H - 2 - (Math.min(p.z, top) / top) * (H - 4)}`).join(" ");
  const y = H - 2 - (4 / top) * (H - 4);
  return <svg className="live-spark" viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" role="img" aria-label="Сигналы эфира: чат и звук"><line className="live-line" x1="0" x2={W} y1={y} y2={y} />{points.length > 1 && <path className="live-z" d={path} />}</svg>;
}

export default function Live() {
  const read = useCallback(() => api.liveState(), []);
  const { data, error, refresh } = useFactoryData(read);
  const [url, setUrl] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [signals, setSignals] = useState<Record<number, LiveSignal[]>>({});
  const last = useRef<Record<number, LiveSignal>>({});
  useEffect(() => onEvent("live-changed", () => void refresh()), [refresh]);
  useEffect(() => onEvent<LiveSignal>("live-signal", s => { last.current[s.sessionId] = s; setSignals(p => ({ ...p, [s.sessionId]: [...(p[s.sessionId] ?? []).slice(-(KEEP - 1)), s] })); }), []);
  const act = async (fn: () => Promise<string | void>) => { setBusy(true); setMessage(""); try { const m = await fn(); if (m) setMessage(m); await refresh(); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } };
  const running = data?.sessions.filter(s => s.state === "live" || s.state === "starting").length ?? 0;

  return <div className="factory-page live-page">
    <FactoryHeading title="Эфиры" detail="Слежу за идущим эфиром: чат и звук подсказывают момент, ролик вырезается из записи без перекодирования и уходит в очередь." />
    <LoadError error={error} retry={refresh} />
    {message && <p className="analytics-notice" role="status">{message}</p>}
    {data && <>
      {!data.flowEnabled && <p className="analytics-notice">Поток «Эфиры» выключен текущей сценой: отмеченные эфиры сами не запускаются. Ручной запуск ниже работает всегда.</p>}
      <section className="factory-panel">
        <h2>Следить за эфиром</h2>
        <p>Идёт {running} из {data.maxSessions}. Свободно на диске: {data.diskFreeMb == null ? "—" : `${Math.round(data.diskFreeMb / 1024)} ГБ`}. Запись держит последние ~20 минут (около 1 ГБ на эфир). Чат читается только на Twitch; на остальных площадках решает звук. Найденные моменты идут в канал «Тренды (песочница)» в режиме проверки.</p>
        <form className="factory-actions" onSubmit={e => { e.preventDefault(); void act(async () => { await api.liveStart(url); setUrl(""); return "Слежение запущено"; }); }}>
          <div style={{ flex: "1 1 320px" }}><Field label="Ссылка на эфир"><input className="factory-input" placeholder="https://twitch.tv/имя" value={url} onChange={e => setUrl(e.target.value)} /></Field></div>
          <button className="factory-primary" disabled={busy || !url.trim() || running >= data.maxSessions}>Следить</button>
        </form>
      </section>

      <h2 style={{ marginTop: 24, fontSize: 16, fontWeight: 600 }}>Сессии</h2>
      {data.sessions.length ? <div className="live-sessions">{data.sessions.map(s => {
        const on = s.state === "live" || s.state === "starting";
        const pts = signals[s.id] ?? [];
        const lastSig = pts[pts.length - 1];
        return <article className="trend-card" key={s.id}>
          <div className="trend-meta"><span className="trend-chip"><span className="live-dot" data-state={s.state} />{STATE[s.state]}</span><span className="trend-chip">{PLATFORM[s.platform] ?? s.platform}</span><span className="trend-chip">{s.chat ? "чат + звук" : "только звук"}</span>{s.manual && <span className="trend-chip">вручную</span>}</div>
          <h3>{s.channel || s.title || s.url}</h3>
          <div className="trend-meta"><span>{on ? `идёт ${dur(Math.floor(Date.now() / 1000) - s.startedAt)}` : `было ${dur((s.endedAt ?? s.startedAt) - s.startedAt)}`}</span><span>·</span><span>моментов: {s.moments}</span>{s.note && !on && <span>· {s.note}</span>}</div>
          {on && <><Spark points={pts} />{lastSig && <div className="trend-meta"><span>чат {Math.round(lastSig.msgs)} сообщ./интервал</span><span>·</span><span>{Math.round(lastSig.authors)} авторов</span><span>·</span><span>звук {Math.round(lastSig.audioDb)} дБ</span></div>}</>}
          <div className="factory-actions">
            {on && <button className="factory-primary" disabled={busy} onClick={() => void act(async () => { await api.liveCutNow(s.id, 45); return "Режу последние 45 секунд…"; })}>Вырезать сейчас</button>}
            {on && <button className="factory-secondary" disabled={busy} onClick={() => void act(() => api.liveStop(s.id))}>Остановить</button>}
            <a className="factory-link" href={s.url} target="_blank" rel="noreferrer">Открыть эфир</a>
          </div>
        </article>;
      })}</div> : <p className="factory-empty">Сессий пока нет. Вставьте ссылку выше или отметьте эфир «Следить» на экране «Тренды».</p>}

      <section className="factory-panel" style={{ marginTop: 20 }}>
        <h2>Найденные моменты</h2>
        {data.moments.length ? <ul className="node-items">{data.moments.map(m => <li key={m.id}>
          <strong>{m.state === "failed" ? "Не вырезан" : m.state === "ignored" ? "Игнорирован" : m.manual ? "Ручной момент" : `Момент, сила ${m.score.toFixed(1)}`} · {Math.round(m.length)} с</strong>
          <small>{new Date(m.createdAt * 1000).toLocaleTimeString("ru-RU")} · {data.sessions.find(s => s.id === m.sessionId)?.channel || "эфир"}{m.error ? ` · ${m.error}` : m.itemId ? " · в очереди фабрики" : ""}</small>
          {m.state === "cut" && <button className="factory-link" onClick={() => void act(() => api.liveMomentIgnore(m.id))}>Игнорировать</button>}
        </li>)}</ul> : <p className="factory-empty">Моментов ещё нет.</p>}
      </section>
    </>}
  </div>;
}
