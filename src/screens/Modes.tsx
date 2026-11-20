import { useCallback, useState } from "react";
import { api, errText, type FlowId, type Placement, type RuleCond, type RuleDto, type SceneDto } from "../lib/api";
import { FactoryHeading, Field, LoadError, useFactoryData } from "../components/FactoryControls";
import { onEvent } from "../lib/events";
import { useEffect } from "react";

const FLOWS: Record<FlowId, string> = { live: "Эфиры (live-нарезка)", studio: "Ручная Студия", discovery: "Автопоиск трендов", monitor: "Мониторинг каналов", publish: "Публикация по календарю", offload: "Приём задач с ПК" };
const PLACE: Record<Placement, string> = { pc: "ПК", server: "Сервер", auto: "Авто" };
const NODE = { pc: "ПК", server: "сервер" } as const;
const condText = (c: RuleCond) => c.kind === "pc_online" ? (c.value ? "ПК включён" : "ПК выключен") : c.kind === "hours" ? `время ${c.from}:00–${c.to}:00` : c.kind === "server_busy" ? "сервер под нагрузкой" : c.kind === "disk_low" ? "мало места на диске" : "стример вышел в эфир";

export default function Modes() {
  const read = useCallback(() => api.modesState(), []);
  const { data, error, refresh } = useFactoryData(read);
  const [message, setMessage] = useState("");
  const [edit, setEdit] = useState<SceneDto | null>(null);
  useEffect(() => onEvent("scene-changed", () => void refresh()), [refresh]);
  const act = async (fn: () => Promise<string | void>) => { setMessage(""); try { const m = await fn(); if (m) setMessage(m); await refresh(); } catch (e) { setMessage(errText(e)); } };
  const active = data?.scenes.find(s => s.id === data.active);
  const pinned = data && data.pinnedUntil * 1000 > Date.now();
  const sceneName = (id: number) => data?.scenes.find(s => s.id === id)?.name ?? "—";

  return <div className="factory-page modes-page">
    <FactoryHeading title="Режимы" detail="Что работает одновременно. Сцена включает нужные потоки и решает, где они считаются." />
    <LoadError error={error} retry={refresh} />
    {message && <p className="analytics-notice" role="status">{message}</p>}
    {data && <>
      <section className="factory-panel">
        <div className="factory-section-heading"><h2>Сцены</h2>{pinned && <button className="factory-link" onClick={() => void act(() => api.sceneUnpin())}>Снять закрепление</button>}</div>
        <div className="factory-actions">{data.scenes.map(s => <button key={s.id} className="factory-secondary" aria-pressed={s.id === data.active} style={s.id === data.active ? { borderColor: "var(--accent)" } : undefined} onClick={() => void act(async () => { await api.sceneActivate(s.id); return `Сцена «${s.name}» включена${"" } на 4 часа вручную: правила её не переключают`; })}>{s.name}</button>)}</div>
        <p>{active ? <>Сейчас: <strong>{active.name}</strong>{pinned ? " (закреплена вручную)" : ""}. Работающие задачи не обрываются: выключенные потоки просто не берут новую работу.</> : "Сцена ещё не выбрана: работает как раньше (мониторинг, Студия, публикация)."}</p>
      </section>

      <section className="factory-panel">
        <h2>Что запущено</h2>
        {data.actions.length === 0 ? <p className="factory-empty">Выберите сцену.</p> : <ul className="node-items">{data.actions.map(a => <li key={a.flow}>
          <strong>{FLOWS[a.flow]}</strong>
          <small>{a.run ? `● ${a.node ? NODE[a.node] : ""}${a.limit ? ` · не более ${a.limit}` : ""}` : "○ не запущен"} · {a.reason}</small>
        </li>)}</ul>}
      </section>

      {edit ? <form className="factory-panel factory-form" onSubmit={e => { e.preventDefault(); void act(async () => { await api.sceneSave(edit); setEdit(null); return "Сцена сохранена"; }); }}>
        <h2>{edit.id ? "Сцена" : "Новая сцена"}</h2>
        <Field label="Название"><input className="factory-input" value={edit.name} onChange={e => setEdit({ ...edit, name: e.target.value })} required /></Field>
        <table className="modes-matrix"><thead><tr><th>Поток</th><th>Включён</th><th>Где</th></tr></thead><tbody>{edit.flows.map((f, i) => <tr key={f.flow}>
          <td>{FLOWS[f.flow]}</td>
          <td><input type="checkbox" checked={f.enabled} aria-label={`${FLOWS[f.flow]}: включён`} onChange={e => setEdit({ ...edit, flows: edit.flows.map((x, j) => j === i ? { ...x, enabled: e.target.checked } : x) })} /></td>
          <td><select className="factory-input" value={f.placement} onChange={e => setEdit({ ...edit, flows: edit.flows.map((x, j) => j === i ? { ...x, placement: e.target.value as Placement } : x) })}>{(Object.keys(PLACE) as Placement[]).map(p => <option key={p} value={p}>{PLACE[p]}</option>)}</select></td>
        </tr>)}</tbody></table>
        <div className="factory-actions"><button className="factory-secondary">Сохранить</button><button type="button" className="factory-link" onClick={() => setEdit(null)}>Отмена</button></div>
      </form> : <div className="factory-actions">
        {active && <button className="factory-secondary" onClick={() => setEdit(structuredClone(active))}>Изменить «{active.name}»</button>}
        <button className="factory-secondary" onClick={() => setEdit({ id: 0, name: "", builtin: false, flows: (Object.keys(FLOWS) as FlowId[]).map(flow => ({ flow, enabled: false, placement: "auto" as Placement })) })}>Новая сцена</button>
        {active && !active.builtin && <button className="factory-link" onClick={() => void act(async () => { await api.sceneDelete(active.id); })}>Удалить</button>}
      </div>}

      <section className="factory-panel">
        <h2>Правила автопереключения</h2>
        <p>Если условие выполняется, включается сцена. Ручной выбор закрепляет сцену на 4 часа.</p>
        <ul className="node-items">{data.rules.map((r: RuleDto) => <li key={r.id}>
          <label className="factory-check"><input type="checkbox" checked={r.enabled} onChange={e => void act(() => api.ruleSave({ ...r, enabled: e.target.checked }))} /><strong>{r.name}</strong></label>
          <small>если {condText(r.cond)} → «{sceneName(r.sceneId)}»</small>
          <button className="factory-link" onClick={() => void act(() => api.ruleDelete(r.id))}>Удалить</button>
        </li>)}</ul>
        {!data.rules.length && <p className="factory-empty">Правил нет.</p>}
      </section>

      <section className="factory-panel"><h2>Журнал переключений</h2>
        {data.log.length ? <ul className="node-items">{data.log.map(([at, why], i) => <li key={i}><small>{new Date(at * 1000).toLocaleString("ru-RU")}</small> {why}</li>)}</ul> : <p className="factory-empty">Переключений ещё не было.</p>}
      </section>
    </>}
  </div>;
}
