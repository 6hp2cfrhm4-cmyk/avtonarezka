import { useCallback, useEffect, useState } from "react";
import { api, errText, type TrendCandidate, type TrendConfig } from "../lib/api";
import { FactoryHeading, Field, LoadError, useFactoryData } from "../components/FactoryControls";
import { onEvent } from "../lib/events";

const KIND = { live: "Эфир", vod: "Запись", clip: "Клип" } as const;
const PLATFORM: Record<string, string> = { twitch: "Twitch", kick: "Kick", youtube: "YouTube" };
const PROVIDERS: { id: string; label: string; flag?: "twitch" | "kick" | "youtube" | "topics" }[] = [
  { id: "twitch", label: "Twitch: топ эфиров и клипы суток", flag: "twitch" },
  { id: "kick", label: "Kick: эфиры по зрителям", flag: "kick" },
  { id: "youtube", label: "YouTube: популярное", flag: "youtube" },
  { id: "topics", label: "Темы дня: Google Trends, Wikipedia", flag: "topics" },
];
const num = (n: number) => n.toLocaleString("ru-RU", { maximumFractionDigits: 0 });
const list = (s: string) => s.split(",").map(x => x.trim()).filter(Boolean);
const ago = (t: number) => { if (!t) return "ещё не опрашивали"; const m = Math.max(0, Math.round((Date.now() / 1000 - t) / 60)); return m < 1 ? "только что" : m < 60 ? `${m} мин назад` : `${Math.round(m / 60)} ч назад`; };

function Card({ c, onAccept, onReject, busy }: { c: TrendCandidate; onAccept: () => void; onReject: () => void; busy: boolean }) {
  const growth = c.previous > 0 ? Math.round(((c.metric - c.previous) / c.previous) * 100) : null;
  return <article className="trend-card">
    <div className="trend-meta"><span className="trend-chip" data-kind={c.kind}>{KIND[c.kind]}</span><span className="trend-chip">{PLATFORM[c.platform] ?? c.platform}</span>{c.category && <span className="trend-chip">{c.category}</span>}</div>
    <h3>{c.title || c.url}</h3>
    <div className="trend-meta"><span>{c.channelName}</span><span>·</span><span>{num(c.metric)} {c.kind === "live" ? "зрителей" : "просмотров"}</span>{growth != null && <span>· {growth > 0 ? "+" : ""}{growth} % за {num(c.windowMin)} мин</span>}</div>
    <div className="trend-score" role="img" aria-label={`Оценка ${Math.round(c.score * 100)} из 100`}><span style={{ width: `${Math.round(c.score * 100)}%` }} /></div>
    <p className="trend-why">{c.why || "Нет роста за окно: оценка по размеру и совпадениям"}</p>
    <div className="factory-actions">
      <button className="factory-primary" disabled={busy} onClick={onAccept}>{c.kind === "live" ? "Следить за эфиром" : "В очередь"}</button>
      <button className="factory-secondary" disabled={busy} onClick={onReject}>Отклонить</button>
      <a className="factory-link" href={c.url} target="_blank" rel="noreferrer">Открыть</a>
    </div>
  </article>;
}

export default function Trends() {
  const read = useCallback(() => api.discoveryState(), []);
  const { data, error, refresh } = useFactoryData(read);
  const [cfg, setCfg] = useState<TrendConfig | null>(null);
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [keys, setKeys] = useState({ youtube: "", kick: "" });
  useEffect(() => onEvent("discovery-changed", () => void refresh()), [refresh]);
  useEffect(() => { if (data && !cfg) setCfg(data.config); }, [data, cfg]);
  const act = async (fn: () => Promise<string | void>) => { setBusy(true); setMessage(""); try { const m = await fn(); if (m) setMessage(m); await refresh(); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } };
  const saveCfg = (next: TrendConfig) => { setCfg(next); void act(async () => { setCfg(await api.discoveryConfigSet(next)); }); };

  return <div className="factory-page trends-page">
    <FactoryHeading title="Тренды" detail="Что сейчас набирает популярность. Оценка идёт по росту за последние минуты, а не по абсолютным цифрам.">
      <div className="factory-actions"><button className="factory-secondary" disabled={busy} onClick={() => void act(async () => { const s = await api.discoveryPollNow(); return `Просмотрено ${s.seen}, новых в списке ${s.fresh}${s.autoAccepted ? `, принято по правилу ${s.autoAccepted}` : ""}`; })}>Опросить сейчас</button></div>
    </FactoryHeading>
    <LoadError error={error} retry={refresh} />
    {message && <p className="analytics-notice" role="status">{message}</p>}
    {data && cfg && <>
      {!data.flowEnabled && <p className="analytics-notice">Поток «Автопоиск трендов» выключен текущей сценой, поэтому сам по таймеру не опрашивает. Включите его на экране «Режимы» или нажмите «Опросить сейчас».</p>}
      <p className="analytics-notice">Последний опрос: {ago(data.lastPoll)}. Всё найденное попадает сюда, а принятые записи идут в канал «Тренды (песочница)» в режиме проверки: без вашего одобрения ничего не публикуется.</p>

      <div className="trend-cols">
        <section className="factory-panel"><h2>Источники</h2>
          {PROVIDERS.map(p => { const st = data.providers[p.id]; const on = cfg[p.flag!]; return <div className="trend-provider" key={p.id}>
            <label className="factory-check"><input type="checkbox" checked={on} onChange={e => saveCfg({ ...cfg, [p.flag!]: e.target.checked })} />{p.label}</label>
            <span className="trend-state" data-ok={String(st?.ok)}>{!on ? "выключено" : !st?.configured ? "нужен ключ" : st.ok === false ? st.message : st.ok ? `найдено ${st.found}` : "ждёт опроса"}</span>
          </div>; })}
          <div className="factory-form" style={{ marginTop: 12, display: "grid", gap: 10 }}>
            <p>Twitch: Client ID и Secret задаются в настройках анализа{data.twitchKeys ? " (уже сохранены)" : ""}.</p>
            <Field label="Ключ YouTube Data API" hint={data.youtubeKey ? "Сохранён. Пустое значение удалит." : "Бесплатный ключ из Google Cloud."}><input className="factory-input" type="password" autoComplete="off" value={keys.youtube} onChange={e => setKeys({ ...keys, youtube: e.target.value })} /></Field>
            <Field label="Токен Kick" hint={data.kickToken ? "Сохранён. Пустое значение удалит." : "Токен приложения Kick (официальный API)."}><input className="factory-input" type="password" autoComplete="off" value={keys.kick} onChange={e => setKeys({ ...keys, kick: e.target.value })} /></Field>
            <div className="factory-actions"><button className="factory-secondary" disabled={busy || (!keys.youtube && !keys.kick)} onClick={() => void act(async () => { if (keys.youtube) await api.discoverySecretSet("youtube", keys.youtube); if (keys.kick) await api.discoverySecretSet("kick", keys.kick); setKeys({ youtube: "", kick: "" }); return "Ключи сохранены в защищённом хранилище"; })}>Сохранить ключи</button></div>
          </div>
        </section>

        <section className="factory-panel"><h2>Что искать</h2>
          <div className="factory-form" style={{ display: "grid", gap: 10 }}>
            <Field label="Языки" hint="Через запятую, например ru, en. Первый используется для запросов."><input className="factory-input" defaultValue={cfg.languages.join(", ")} onBlur={e => saveCfg({ ...cfg, languages: list(e.target.value) })} /></Field>
            <Field label="Категории и игры" hint="Совпадение повышает оценку."><input className="factory-input" defaultValue={cfg.categories.join(", ")} onBlur={e => saveCfg({ ...cfg, categories: list(e.target.value) })} /></Field>
            <Field label="Ключевые слова в названии"><input className="factory-input" defaultValue={cfg.keywords.join(", ")} onBlur={e => saveCfg({ ...cfg, keywords: list(e.target.value) })} /></Field>
            <Field label="Ваши каналы" hint="Часть ссылки или имя; такие источники оцениваются выше."><input className="factory-input" defaultValue={cfg.knownChannels.join(", ")} onBlur={e => saveCfg({ ...cfg, knownChannels: list(e.target.value) })} /></Field>
          </div>
          {data.topics.length > 0 && <><p>Темы дня (подмешиваются в оценку):</p><div className="trend-topics">{data.topics.slice(0, 12).map(t => <span className="trend-chip" key={t}>{t}</span>)}</div></>}
        </section>

        <section className="factory-panel"><h2>Автопринятие</h2>
          <p>Само ставит в очередь только то, что выше порога и из белого списка (или если у песочницы указано разрешение «моё» или «договорённость»). Иначе — только сюда, на ваш выбор.</p>
          <div className="factory-form" style={{ display: "grid", gap: 10 }}>
            <Field label={`Порог оценки: ${Math.round(cfg.threshold * 100)}`}><input className="trend-range" type="range" min={30} max={100} value={Math.round(cfg.threshold * 100)} onChange={e => setCfg({ ...cfg, threshold: Number(e.target.value) / 100 })} onMouseUp={() => saveCfg(cfg)} onKeyUp={() => saveCfg(cfg)} onTouchEnd={() => saveCfg(cfg)} /></Field>
            <Field label="Не больше новых задач в сутки" hint="0 — автопринятие выключено."><input className="factory-input" type="number" min={0} max={100} value={cfg.dailyLimit} onChange={e => setCfg({ ...cfg, dailyLimit: Number(e.target.value) })} onBlur={() => saveCfg(cfg)} /></Field>
            <Field label="Белый список" hint="Каналы или ссылки, которым разрешено автопринятие."><input className="factory-input" defaultValue={cfg.whitelist.join(", ")} onBlur={e => saveCfg({ ...cfg, whitelist: list(e.target.value) })} /></Field>
            <Field label="Опрос раз в, минут"><input className="factory-input" type="number" min={5} max={240} value={cfg.intervalMin} onChange={e => setCfg({ ...cfg, intervalMin: Number(e.target.value) })} onBlur={() => saveCfg(cfg)} /></Field>
          </div>
        </section>
      </div>

      <h2 style={{ marginTop: 24, fontSize: 16, fontWeight: 600 }}>Кандидаты ({data.new.length})</h2>
      {data.new.length ? <div className="trend-grid">{data.new.map(c => <Card key={c.id} c={c} busy={busy}
        onAccept={() => void act(async () => { const id = await api.discoveryAccept(c.id); return id == null ? "Эфир отмечен: за ним последит модуль эфиров" : "Поставлено в очередь фабрики"; })}
        onReject={() => void act(() => api.discoveryReject(c.id))} />)}</div>
        : <p className="factory-empty">Пока ничего достойного. Подключите источники и нажмите «Опросить сейчас».</p>}

      {(data.watch.length > 0 || data.accepted.length > 0) && <section className="factory-panel" style={{ marginTop: 20 }}>
        {data.watch.length > 0 && <><h2>Следим за эфирами</h2><ul className="node-items">{data.watch.map(c => <li key={c.id}><strong>{c.title}</strong><small>{PLATFORM[c.platform]} · {c.channelName}</small></li>)}</ul></>}
        {data.accepted.length > 0 && <><h2>Принято</h2><ul className="node-items">{data.accepted.slice(0, 15).map(c => <li key={c.id}><strong>{c.title}</strong><small>{PLATFORM[c.platform]} · {c.channelName}{c.auto ? " · по правилу" : ""}</small></li>)}</ul></>}
      </section>}
    </>}
  </div>;
}
