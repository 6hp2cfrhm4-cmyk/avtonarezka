import { useCallback, useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { api, errText, fmtDuration, inTauri, type BatchItem, type BatchStage, type InboxItem } from "../lib/api";
import { useUi } from "../store";
import { FactoryHeading, Field, LoadError, useFactoryData } from "../components/FactoryControls";
import VirtualRows from "../components/VirtualRows";
import { useMediaUrl } from "../lib/useMediaUrl";
const read = async () => ({ state: await api.factoryState(), channels: await api.channelsList() });
export const stageLabel: Record<BatchStage, string> = {
  discovered: "Обнаружено", queued: "В очереди", fetching: "Получаю видео", downloading: "Загрузка", importing: "Импорт",
  scouting: "Анализ", analyzing: "Анализ", selecting: "Отбор", awaiting_selection: "Выбор в Студии", drafting: "Превью", qc: "Проверка качества",
  awaiting_approval: "Ждёт одобрения", auto_approved: "Одобрено автоматически", rendering_final: "Финальный рендер", rendering: "Финальный рендер",
  captioning: "Подпись", ready_to_schedule: "Готово к планированию", scheduled: "Запланировано", publishing: "Отправка", published: "Опубликовано",
  measured: "Есть метрики", completed: "Готово", done: "Готово", failed: "Ошибка", needs_check: "Проверь результат отправки",
};
const columns: { title: string; stages: BatchStage[] }[] = [
  { title: "Источники", stages: ["discovered", "queued"] },
  { title: "В работе", stages: ["fetching", "scouting", "selecting", "drafting", "qc", "auto_approved", "rendering_final", "captioning"] },
  { title: "Inbox", stages: ["awaiting_approval", "awaiting_selection", "failed", "needs_check"] },
  { title: "Календарь", stages: ["ready_to_schedule", "scheduled", "publishing"] },
  { title: "Готово", stages: ["completed", "published", "measured", "done"] },
];
async function openStudio(item: BatchItem) {
  if (!item.projectId) return;
  const projects = await api.listProjects(); const project = projects.find(p => p.id === item.projectId);
  if (project) { const ui = useUi.getState(); ui.setProjects(projects); ui.setProject(project); ui.go(item.stage === "awaiting_selection" ? "moments" : "drafts"); }
}
function InboxCard({ item, selected, onSelect }: { item: InboxItem; selected: boolean; onSelect: () => void }) {
  const image = useMediaUrl(item.draft.posterPath, item.draft.updatedAt);
  const video = useMediaUrl(item.draft.proxyPath, item.draft.updatedAt);
  return <article className={`factory-inbox-card ${selected ? "is-selected" : ""}`}>
    <label className="factory-check"><input type="checkbox" checked={selected} onChange={onSelect} />{item.channelName} · {item.draft.part}</label>
    <div className="factory-clip-preview">{video ? <video src={video} poster={image ?? undefined} controls muted preload="none" /> : image ? <img src={image} alt="Превью черновика" /> : <span>Превью недоступно</span>}</div>
    <strong title={item.draft.title}>{item.draft.title || `Часть ${item.draft.part}`}</strong>
    <small>{fmtDuration(item.draft.end - item.draft.start)}</small>
    <div className={`factory-qc ${item.qc?.issues.length ? "has-issues" : ""}`}>{item.draft.error || (item.qc ? item.qc.issues.length ? item.qc.issues.join(" · ") : "QC пройден" : "Ожидает QC")}</div>
    <button className="factory-link" onClick={() => void openStudio({ projectId: item.draft.projectId, stage: "awaiting_approval" } as BatchItem).catch(e => useUi.getState().notify(errText(e), "error"))}>Открыть в Студии</button>
  </article>;
}
function InboxGrid({ items, selected, toggle }: { items: InboxItem[]; selected: number[]; toggle: (id: number) => void }) {
  const ref = useRef<HTMLDivElement>(null); const [cols, setCols] = useState(3);
  useEffect(() => { if (!ref.current) return; const ob = new ResizeObserver(([entry]) => setCols(Math.max(1, Math.min(4, Math.floor(entry.contentRect.width / 255))))); ob.observe(ref.current); return () => ob.disconnect(); }, []);
  const rows = Array.from({ length: Math.ceil(items.length / cols) }, (_, i) => items.slice(i * cols, (i + 1) * cols));
  return <div ref={ref}><VirtualRows items={rows} virtualize={items.length > 24} rowHeight={370} height={600} label="Ролики на одобрение" render={row => <div className="factory-inbox-row" style={{ gridTemplateColumns: `repeat(${cols}, minmax(0,1fr))` }}>{row.map(item => <InboxCard key={item.draft.id} item={item} selected={selected.includes(item.draft.id)} onSelect={() => toggle(item.draft.id)} />)}</div>} /></div>;
}
export default function Factory() {
  const { data, error, refresh } = useFactoryData(read);
  const [selected, setSelected] = useState<number[]>([]); const [channelId, setChannelId] = useState(""); const [source, setSource] = useState("");
  const [busy, setBusy] = useState(false); const [message, setMessage] = useState("");
  const notify = useUi(s => s.notify);
  const act = useCallback(async (fn: () => Promise<unknown>) => { setBusy(true); setMessage(""); try { await fn(); await refresh(); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } }, [refresh]);
  useEffect(() => { if (!data?.state.running) return; const timer = setInterval(() => void refresh(), 2000); return () => clearInterval(timer); }, [data?.state.running, refresh]);
  const decide = useCallback((approve: boolean) => {
    if (!selected.length || busy) return;
    if (approve && selected.some(id => !data?.state.inbox.some(i => i.draft.id === id && i.draft.status === "ready" && i.qc))) { setMessage("Подожди завершения QC выбранных черновиков"); return; }
    void act(async () => { const n = await api.factoryDecide(selected, approve); setSelected([]); notify(`${approve ? "Одобрено" : "Отклонено"}: ${n}`); });
  }, [selected, busy, act, notify, data?.state.inbox]);
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement)?.closest("input:not([type=checkbox]),textarea,select,video,[contenteditable]")) return;
      if (e.key === "ArrowRight") { e.preventDefault(); decide(true); }
      if (e.key === "ArrowLeft") { e.preventDefault(); decide(false); }
      if (e.code === "KeyA" && !e.ctrlKey && !e.metaKey) { e.preventDefault(); setSelected(data?.state.inbox.map(i => i.draft.id) ?? []); }
    }; window.addEventListener("keydown", key); return () => window.removeEventListener("keydown", key);
  }, [decide, data?.state.inbox]);
  const addFiles = () => void act(async () => {
    if (!channelId) throw new Error("Выбери профиль канала");
    const paths = await open({ multiple: true, filters: [{ name: "Видео", extensions: ["mp4", "mov", "mkv", "webm", "avi", "m4v"] }] });
    if (paths) await api.batchAdd((Array.isArray(paths) ? paths : [paths]).map(path => ({ kind: "file", source: path, channelId: +channelId })));
  });
  const start = () => void act(() => api.batchStart());
  const canApprove = selected.length > 0 && selected.every(id => data?.state.inbox.some(i => i.draft.id === id && i.draft.status === "ready" && i.qc));
  const pending = data?.state.items.filter(i => !["completed", "published", "measured", "done", "ready_to_schedule", "scheduled"].includes(i.stage)).length ?? 0;
  return <div className="factory-page">
    <FactoryHeading title="Фабрика" detail={`${data?.state.items.length ?? 0} источников · ${pending} требуют обработки · ${data?.state.inbox.length ?? 0} роликов ждут решения`}>
      <div className="factory-actions"><span>{data?.state.paused ? "На паузе" : data?.state.running ? "Работает" : "Ожидает запуска"}</span><button className="factory-primary" disabled={busy || !inTauri} onClick={start}>{data?.state.paused ? "Продолжить" : "Запустить"}</button><button className="factory-secondary" title="Текущий этап завершится, следующие будут ждать. Пауза сохраняется после перезапуска." disabled={busy || !!data?.state.paused} onClick={() => void act(() => api.batchCancel())}>Пауза</button></div>
    </FactoryHeading>
    {!inTauri && <p className="factory-notice">Демонстрация интерфейса. Обработка видео работает в настольном приложении.</p>}
    <LoadError error={error} retry={refresh} />{message && <div role="alert" className="factory-error">{message}</div>}
    <details className="factory-panel factory-add" open={!data?.state.items.length}><summary>Добавить источники</summary><div className="factory-add-grid">
      <Field label="Профиль канала"><select className="factory-input" value={channelId} onChange={e => setChannelId(e.target.value)}><option value="">Выбери канал</option>{data?.channels.map(c => <option key={c.id} value={c.id} disabled={!c.enabled}>{c.name}{!c.enabled ? " · пауза" : ""}</option>)}</select></Field>
      <Field label="Ссылка на запись YouTube / Twitch / Kick"><input className="factory-input" value={source} onChange={e => setSource(e.target.value)} placeholder="Ссылка на одну запись" /></Field>
      <div className="factory-actions"><button className="factory-secondary" disabled={busy || !channelId || !source.trim() || !inTauri} onClick={() => void act(async () => { await api.batchAdd([{ kind: "link", source, channelId: +channelId }]); setSource(""); })}>Добавить ссылку</button><button className="factory-secondary" disabled={busy || !channelId || !inTauri} onClick={addFiles}>Выбрать файлы</button></div>
    </div>{data?.channels.length === 0 && <button className="factory-link" onClick={() => useUi.getState().setArea("channels")}>Создать первый профиль канала</button>}</details>
    <section className="factory-board" aria-label="Стадии фабрики">{columns.map(col => {
      const items = data?.state.items.filter(i => col.stages.includes(i.stage)) ?? [];
      return <div key={col.title} className="factory-board-column"><h2>{col.title}<span>{items.length}</span></h2>{items.length === 0 ? <p className="factory-empty">Пока пусто</p> : <VirtualRows items={items} height={240} rowHeight={96} label={col.title} render={item => <div className="factory-board-source"><button className="factory-link" title={item.title || item.source} disabled={!item.projectId} onClick={() => void openStudio(item).catch(e => notify(errText(e), "error"))}>{item.title || item.source.split(/[\\/]/).pop()}</button><small>{item.paused ? "Пауза · " : ""}{stageLabel[item.stage]}</small><div className="factory-source-controls"><label title="Приоритет">↟ <input type="number" min={-100} max={100} aria-label={`Приоритет ${item.title || item.id}`} value={item.priority} onChange={e => void act(() => api.factoryItemUpdate(item.id, +e.target.value, item.position, item.paused))} /></label><label title="Порядок">№ <input type="number" min={0} aria-label={`Порядок ${item.title || item.id}`} value={item.position} onChange={e => void act(() => api.factoryItemUpdate(item.id, item.priority, +e.target.value, item.paused))} /></label><button disabled={busy} onClick={() => void act(() => api.factoryItemUpdate(item.id, item.priority, item.position, !item.paused))}>{item.paused ? "Продолжить" : "Пауза"}</button>{item.stage === "failed" && <button disabled={busy} onClick={() => void act(() => api.batchRetry(item.id))}>Повторить</button>}</div>{item.error && <span className="factory-source-note" title={item.error}>{item.error}</span>}</div>} />}</div>;
    })}</section>
    <section className="factory-panel"><div className="factory-section-heading"><div><h2>Inbox · {data?.state.inbox.length ?? 0}</h2><p>Проверь превью и причины QC перед одобрением.</p></div><div className="factory-actions"><button className="factory-link" onClick={() => setSelected(data?.state.inbox.map(i => i.draft.id) ?? [])}>Выбрать все · A</button><button className="factory-secondary" disabled={busy || !selected.length} onClick={() => decide(false)}>← Отклонить</button><button className="factory-primary" disabled={busy || !canApprove} onClick={() => decide(true)}>Одобрить {selected.length || ""} →</button></div></div>
      {!data?.state.inbox.length ? <p className="factory-empty">Здесь появятся черновики каналов с одобрением и ролики с замечаниями QC.</p> : <InboxGrid items={data.state.inbox} selected={selected} toggle={id => setSelected(s => s.includes(id) ? s.filter(x => x !== id) : [...s, id])} />}
    </section>
    <section className="factory-panel"><div className="factory-section-heading"><h2>Что происходит</h2><button className="factory-link" onClick={() => void refresh()}>Обновить</button></div>{!data?.state.events.length ? <p className="factory-empty">События появятся после добавления первого источника.</p> : <VirtualRows items={data.state.events} rowHeight={48} height={320} label="Журнал фабрики" render={event => <div className="factory-event"><time>{new Date(event.createdAt * 1000).toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" })}</time><span>{event.itemId ? `#${event.itemId} · ` : ""}{event.stage && event.message === event.stage ? stageLabel[event.stage as BatchStage] ?? event.stage : event.message}</span>{event.kind === "attention" && <small>Нужно действие</small>}</div>} />}</section>
  </div>;
}
