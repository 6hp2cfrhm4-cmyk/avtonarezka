import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, errText, type AnalyticsFeedback, type AnalyticsRow, type MetricCounts, type MetricSample } from "../lib/api";
import { onEvent } from "../lib/events";
import { useUi } from "../store";
import { FactoryHeading, Field, LoadError, useFactoryData } from "../components/FactoryControls";
import VirtualRows from "../components/VirtualRows";
import { VkMetricsLink } from "../components/VkMetrics";

const number = (n: number | null | undefined) => n == null ? "—" : n.toLocaleString("ru-RU", { maximumFractionDigits: 1 });
const date = (n: number | null | undefined) => n == null ? "—" : new Date(n * 1000).toLocaleString("ru-RU", { dateStyle: "short", timeStyle: "short" });
const platforms: Record<string, string> = { youtube: "YouTube", instagram: "Instagram", telegram: "Telegram", tiktok: "TikTok", vk: "VK" };
const names: Record<string, string> = { loudness: "Громкость", motion: "Движение", events: "Смех и события", speech: "Речь", chat: "Чат", viewer_clips: "Клипы зрителей", camera: "Камера" };
const counters = [["views", "Просмотры"], ["likes", "Нравится"], ["comments", "Комментарии"], ["shares", "Репосты"]] as const;
const readChannels = api.channelsList;

function ManualMetric({ row, close, updated }: { row: AnalyticsRow; close: () => void; updated: () => void }) {
  const [values, setValues] = useState<Record<keyof MetricCounts, string>>({ views: "", likes: "", comments: "", shares: "" });
  const [at, setAt] = useState(""); const [busy, setBusy] = useState(false); const [message, setMessage] = useState("");
  const [history, setHistory] = useState<MetricSample[]>([]);
  useEffect(() => { let live = true; api.analyticsHistory(row.id).then(v => { if (live) setHistory(v); }).catch(e => { if (live) setMessage(errText(e)); }); return () => { live = false; }; }, [row.id]);
  return <section className="factory-panel analytics-manual" aria-label="Ручной замер">
    <div className="factory-section-heading"><h2>Замер · {row.title}</h2><button type="button" className="factory-link" onClick={close}>Закрыть</button></div>
    <p>{platforms[row.platform]} · {row.account}. Пустое поле означает отсутствие данных. Ноль вводится явно.</p>
    <form className="factory-form" onSubmit={async e => { e.preventDefault(); setBusy(true); setMessage(""); try { const counts = Object.fromEntries(counters.map(([k]) => [k, values[k] === "" ? null : Number(values[k])])) as unknown as MetricCounts; await api.analyticsManual(row.id, at ? Math.floor(new Date(at).getTime() / 1000) : null, counts); setHistory(await api.analyticsHistory(row.id)); setMessage("Замер сохранён"); updated(); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } }}>
      <div className="analytics-count-inputs">{counters.map(([k, label]) => <Field key={k} label={label}><input className="factory-input" type="number" min={0} max={1e12} step={1} value={values[k]} onChange={e => setValues(v => ({ ...v, [k]: e.target.value }))} /></Field>)}</div>
      <Field label="Дата замера" hint="Оставь пустым для текущего времени."><input className="factory-input" type="datetime-local" value={at} onChange={e => setAt(e.target.value)} /></Field>
      <div className="factory-actions"><button className="factory-secondary" disabled={busy || counters.every(([k]) => !values[k])}>{busy ? "Сохраняю…" : "Сохранить замер"}</button><span role="status">{message}</span></div>
    </form>
    <h3>История замеров · {history.length}{history.length === 500 ? " последних" : ""}</h3>
    <VirtualRows items={history.map((v, i) => ({ ...v, id: i }))} rowHeight={64} label="История метрик" render={v => <div className="analytics-history-row"><span>{date(v.observedAt)}<small>{v.source === "manual" ? "Вручную" : "API"}</small></span><span>{counters.map(([k, label]) => `${label}: ${number(v[k])}`).join(" · ")}</span></div>} />
    {!history.length && <p className="factory-empty">Замеров ещё нет.</p>}
  </section>;
}

export default function Analytics() {
  const [days, setDays] = useState(30); const [channelId, setChannelId] = useState<number | null>(null);
  const read = useCallback(() => api.analyticsDashboard(days, channelId), [days, channelId]);
  const { data, error, refresh } = useFactoryData(read); const { data: channels } = useFactoryData(readChannels);
  const feedbackVersion = useRef(0);
  const [feedback, setFeedback] = useState<AnalyticsFeedback | null>(null); const [feedbackError, setFeedbackError] = useState("");
  const [selected, setSelected] = useState<AnalyticsRow | null>(null); const [busy, setBusy] = useState(false); const [message, setMessage] = useState("");
  const [sort, setSort] = useState("recent"); const [attentionOnly, setAttentionOnly] = useState(false);
  const loadFeedback = useCallback(async () => { const version = ++feedbackVersion.current; if (channelId == null) { setFeedback(null); setFeedbackError(""); return; } try { const next = await api.analyticsFeedback(channelId); if (version === feedbackVersion.current) { setFeedback(next); setFeedbackError(""); } } catch (e) { if (version === feedbackVersion.current) setFeedbackError(errText(e)); } }, [channelId]);
  useEffect(() => { void loadFeedback(); }, [loadFeedback]);
  useEffect(() => onEvent("analytics-changed", () => { void refresh(); void loadFeedback(); }), [refresh, loadFeedback]);
  const act = async (fn: () => Promise<string | void>) => { setBusy(true); setMessage(""); try { setMessage((await fn()) ?? "Сохранено"); await refresh(); await loadFeedback(); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } };
  const rows = useMemo(() => {
    const rows = [...(data?.rows ?? [])].filter(r => !attentionOnly || r.error || ["failed", "needs_check"].includes(r.state));
    if (sort !== "recent") rows.sort((a, b) => { const x = a.sample?.views, y = b.sample?.views; return x == null ? y == null ? 0 : 1 : y == null ? -1 : sort === "best" ? y - x : x - y; });
    return rows;
  }, [data, sort, attentionOnly]);
  const hours = data?.hours.filter(h => h.published > 0) ?? []; const max = Math.max(1, ...hours.map(h => h.published));
  return <div className="factory-page analytics-page">
    <FactoryHeading title="Аналитика" detail={`Автонарезка · ${channelId == null ? "Все каналы" : channels?.find(c => c.id === channelId)?.name ?? "Канал"} · последние ${days} дней`}>
      <button className="factory-primary" disabled={busy} onClick={() => void act(async () => { const r = await api.analyticsCollect(); return `Получено замеров: ${r.collected}. ${r.errors.length ? r.errors.join(" · ") : r.skipped ? `Аккаунты ждут снятия лимита: ${r.skipped}.` : "Следующая проверка — по сроку обновления площадки."}`; })}>{busy ? "Проверяю…" : "Собрать метрики"}</button>
    </FactoryHeading>
    <div className="analytics-toolbar"><Field label="Канал"><select className="factory-input" value={channelId ?? ""} onChange={e => { feedbackVersion.current++; setFeedback(null); setFeedbackError(""); setChannelId(e.target.value ? Number(e.target.value) : null); setSelected(null); }}><option value="">Все каналы</option>{channels?.map(c => <option key={c.id} value={c.id}>{c.name}</option>)}</select></Field><Field label="Период"><select className="factory-input" value={days} onChange={e => setDays(Number(e.target.value))}><option value={7}>7 дней</option><option value={30}>30 дней</option><option value={90}>90 дней</option><option value={365}>Год</option></select></Field><label className="factory-check"><input type="checkbox" checked={data?.autoCollect ?? false} disabled={!data || busy} onChange={e => void act(() => api.analyticsAuto(e.target.checked))} />Собирать автоматически</label><small>Проверка: {date(data?.checkedAt)}</small></div>
    <LoadError error={error} retry={refresh} />{message && <p className="analytics-notice" role="status">{message}</p>}
    <section className="analytics-totals" aria-label="Итоги публикаций"><div><span>Просмотры</span><strong className={data?.views == null ? "analytics-missing" : ""}>{number(data?.views)}</strong><small>Замеры у {number(data?.measured)} из {number(data?.published)} публикаций</small></div><div><span>Опубликовано</span><strong>{number(data?.published)}</strong></div><div><span>Медиана просмотров</span><strong className={data?.medianViews == null ? "analytics-missing" : ""}>{number(data?.medianViews)}</strong></div><div><span>Ошибки отправки</span><strong className={data?.errorRate == null ? "analytics-missing" : ""}>{data?.errorRate == null ? "—" : `${(data.errorRate * 100).toFixed(1)}%`}</strong><small>{number(data?.errors)} ошибок среди подтверждённых и ошибочных отправок</small></div></section>
    {/* Two equal columns and one 20px gutter fill the same canvas as the totals and list. */}
    <div className="analytics-overview">
      <section className="factory-panel"><div className="factory-section-heading"><h2>Часы публикаций</h2><small>Местное время</small></div>{hours.length ? <div className="analytics-hours">{hours.map(h => <div key={h.hour} className="analytics-hour"><span>{String(h.hour).padStart(2, "0")}:00</span><span className="analytics-hour-track"><i style={{ width: `${h.published / max * 100}%` }} /></span><strong>{number(h.published)}<small>Медиана: {number(h.medianViews)}</small></strong></div>)}</div> : <p className="factory-empty">После первой публикации появится распределение по часам.</p>}<p>Ширина полосы — число публикаций. Медиана использует доступные последние замеры.</p></section>
      <section className="factory-panel"><div className="factory-section-heading"><h2>Отбор по результатам</h2><span>48 часов</span></div><LoadError error={feedbackError} retry={loadFeedback} />{channelId == null ? <p>Выбери канал для подсказок. Нужно 50 разных роликов с замерами в 48–54 часа после публикации.</p> : <><p>{feedback?.reason ?? "Загружаю результаты…"}</p>{feedback?.suggestions.map(s => <div className="analytics-suggestion" key={s.signal}><span><strong>{names[s.signal] ?? s.signal}: {s.current.toFixed(2)} → {s.proposed.toFixed(2)}</strong><small>{s.sampleSize} роликов · связь {s.correlation.toFixed(2)}</small></span><button className="factory-secondary" disabled={busy} onClick={() => void act(() => api.analyticsApply(channelId, s.signal, s.current))}>Применить</button></div>)}</>}<p>Источники: YouTube Statistics, Instagram Insights и VK video.get. Для Telegram и TikTok доступен ручной замер. Доступ к данным зависит от прав аккаунта.</p></section>
    </div>
    <VkMetricsLink updated={() => { void refresh(); void loadFeedback(); }} />
    {selected && <ManualMetric key={selected.id} row={selected} close={() => setSelected(null)} updated={() => { void refresh(); void loadFeedback(); }} />}
    <section className="factory-panel analytics-publications"><div className="factory-section-heading"><h2>Результаты роликов · {number(rows.length)}</h2><div className="factory-actions"><select aria-label="Порядок роликов" className="factory-input" value={sort} onChange={e => setSort(e.target.value)}><option value="recent">Последние</option><option value="best">Больше просмотров</option><option value="worst">Меньше просмотров</option></select><label className="factory-check"><input type="checkbox" checked={attentionOnly} onChange={e => setAttentionOnly(e.target.checked)} />Требуют внимания · {number(data?.attention)}</label></div></div>
      {!data ? <p className="factory-empty">Загружаю публикации…</p> : !rows.length ? <p className="factory-empty">{attentionOnly ? "Публикаций, требующих внимания, нет." : "Публикаций за этот период пока нет."} <button className="factory-link" onClick={() => useUi.getState().setArea("calendar")}>Открыть календарь</button></p> : <VirtualRows items={rows} rowHeight={114} label="Результаты публикаций" render={r => <article className="analytics-result" data-publication-id={r.id}><div><strong title={r.title}>{r.title}</strong><small>{platforms[r.platform] ?? r.platform} · {r.account} · {r.channel}</small><small>{date(r.sentAt)} · {r.state === "done" ? "Опубликовано" : r.state === "needs_check" ? "Проверь отправку" : r.state === "failed" ? "Ошибка отправки" : "Ещё не опубликовано"}</small>{r.error && <small className="analytics-issue" title={r.error}>{r.error}</small>}</div><div className="analytics-result-count"><strong>{number(r.sample?.views)}</strong><small>просмотров</small><small>{r.sample ? `${date(r.sample.observedAt)} · ${r.sample.source === "manual" ? "вручную" : "API"}` : "Замера нет"}</small></div><div className="analytics-result-actions">{r.state === "done" && r.sentAt != null && <button className="factory-link" onClick={() => setSelected(r)}>Внести замер</button>}{r.url && /^https:\/\//.test(r.url) && <a className="factory-link" href={r.url} target="_blank" rel="noreferrer">Публикация</a>}</div></article>} />}
    </section>
  </div>;
}
