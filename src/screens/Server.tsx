import { useCallback, useEffect, useRef, useState } from "react";
import { api, errText, type NodeCandidate, type NodeHostKey, type NodeStatus, type ServerConfig } from "../lib/api";
import { FactoryHeading, Field, LoadError, useFactoryData } from "../components/FactoryControls";
import { onEvent } from "../lib/events";
import ServerWizard from "../components/ServerWizard";

const EMPTY: ServerConfig = { host: "", port: 22, user: "", keyPath: "", enabled: false };
const mb = (n: number | null | undefined) => (n == null ? "—" : n >= 1024 ? `${(n / 1024).toLocaleString("ru-RU", { maximumFractionDigits: 1 })} ГБ` : `${n} МБ`);
const pct = (n: number | null | undefined) => (n == null ? "—" : `${n.toLocaleString("ru-RU", { maximumFractionDigits: 1 })} %`);
const STAGE: Record<string, string> = { queued: "в очереди", fetching: "скачивание", scouting: "поиск моментов", selecting: "отбор", drafting: "черновики", awaiting_approval: "ждёт одобрения", rendering_final: "рендер", completed: "готово", failed: "ошибка" };

export default function Server() {
  const read = useCallback(() => api.nodeOverview(), []);
  const { data: overview, error, refresh } = useFactoryData(read);
  const [cfg, setCfg] = useState<ServerConfig>(EMPTY);
  const [status, setStatus] = useState<NodeStatus | null>(null);
  const [items, setItems] = useState<NodeCandidate[]>([]);
  const [picked, setPicked] = useState<Set<number>>(new Set());
  const [hostKey, setHostKey] = useState<NodeHostKey | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [confirm, setConfirm] = useState<"send" | "handover" | null>(null);
  const [countdown, setCountdown] = useState<number | null>(null);
  const [progress, setProgress] = useState<Record<number, number>>({});
  const timer = useRef<ReturnType<typeof setInterval> | undefined>(undefined);

  const loaded = useRef(false);
  useEffect(() => { if (overview?.config && !loaded.current) { setCfg(overview.config); loaded.current = true; } }, [overview?.config]);
  const loadItems = useCallback(async () => { try { const list = await api.nodeCandidates(); setItems(list); setPicked(p => new Set([...p].filter(id => list.some(i => i.id === id && i.canMove)))); } catch (e) { setMessage(errText(e)); } }, []);
  useEffect(() => { void loadItems(); return onEvent("factory-changed", () => void loadItems()); }, [loadItems]);
  useEffect(() => onEvent<{ id: number; done: number; total: number }>("node-transfer", e => setProgress(p => ({ ...p, [e.id]: e.total ? e.done / e.total : 0 }))), []);
  useEffect(() => () => clearInterval(timer.current), []);

  const act = async (fn: () => Promise<string | void>) => { setBusy(true); setMessage(""); try { const m = await fn(); if (m) setMessage(m); await refresh(); await loadItems(); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } };
  const live = status ?? overview;
  const sys = status?.remote?.system;
  const movable = items.filter(i => i.canMove);
  const away = items.filter(i => i.ownerNode !== "local");
  const chosen = [...picked];
  const toggle = (id: number) => setPicked(p => { const n = new Set(p); n.has(id) ? n.delete(id) : n.add(id); return n; });

  const startCountdown = () => {
    setCountdown(60);
    timer.current = setInterval(() => setCountdown(c => {
      if (c == null) return c;
      if (c <= 1) { clearInterval(timer.current); void api.nodePoweroff().catch(e => setMessage(errText(e))); return null; }
      return c - 1;
    }), 1000);
  };
  const cancelCountdown = () => { clearInterval(timer.current); setCountdown(null); setMessage("Выключение отменено. Задачи остаются на сервере, ПК включён."); };

  return <div className="factory-page server-page">
    <FactoryHeading title="Сервер" detail="Второй работник: принимает задачи, пока ПК выключен, и возвращает готовые ролики.">
      <div className="factory-actions">
        <button className="factory-secondary" disabled={busy || !overview?.configured} onClick={() => void act(async () => { const s = await api.nodeCheck(); setStatus(s); return s.online ? "Связь есть" : s.error ?? "Нет связи"; })}>Проверить связь</button>
        <button className="factory-secondary" disabled={busy || !overview?.enabled} onClick={() => void act(async () => { const r = await api.nodePull(); return r.finished.length || r.failed.length ? `Готово: ${r.finished.length}, с ошибкой: ${r.failed.length}` : `Ждут сервера: ${r.waiting}`; })}>Забрать результаты</button>
      </div>
    </FactoryHeading>
    <LoadError error={error} retry={refresh} />
    {message && <p className="analytics-notice" role="status">{message}</p>}
    {countdown != null && <div className="disk-confirm" role="alert"><p>Всё передано и подтверждено сервером. ПК выключится через <strong>{countdown}</strong> с.</p><div className="factory-actions"><button className="factory-secondary" onClick={cancelCountdown}>Отменить выключение</button></div></div>}

    <ServerWizard cfg={cfg} onKey={keyPath => setCfg(c => ({ ...c, keyPath }))} />

    <div className="disk-overview">
      <form className="factory-panel factory-form" onSubmit={e => { e.preventDefault(); void act(async () => { await api.nodeConfigSet(cfg); return "Настройки сохранены"; }); }}>
        <h2>Подключение</h2>
        <div className="factory-fields">
          <Field label="Адрес"><input className="factory-input" value={cfg.host} onChange={e => setCfg({ ...cfg, host: e.target.value })} placeholder="адрес сервера или example.com" /></Field>
          <Field label="Порт"><input type="number" className="factory-input" value={cfg.port} onChange={e => setCfg({ ...cfg, port: +e.target.value })} /></Field>
          <Field label="Пользователь"><input className="factory-input" value={cfg.user} onChange={e => setCfg({ ...cfg, user: e.target.value })} /></Field>
          <Field label="Файл ключа" hint="Отдельный ключ проекта (после установки на сервер он ограничен: только канал задач, без shell)."><input className="factory-input" value={cfg.keyPath} onChange={e => setCfg({ ...cfg, keyPath: e.target.value })} placeholder="создаётся кнопкой в мастере ниже" /></Field>
        </div>
        <label className="factory-check"><input type="checkbox" checked={cfg.enabled} onChange={e => setCfg({ ...cfg, enabled: e.target.checked })} />Связь с сервером включена</label>
        <div className="factory-actions">
          <button className="factory-secondary" disabled={busy}>Сохранить</button>
          <button type="button" className="factory-secondary" disabled={busy || !overview?.configured} onClick={() => void act(async () => { setHostKey(await api.nodeHostkeyScan()); })}>Проверить отпечаток сервера</button>
        </div>
        {hostKey && <div className="disk-confirm" role="alert"><p>Отпечаток сервера: <code>{hostKey.fingerprint}</code></p><p>Сверьте его с тем, что показывает ваш хостинг. Доверять нужно только если он совпадает.</p><div className="factory-actions"><button type="button" className="factory-secondary" onClick={() => void act(async () => { await api.nodeHostkeyAccept(hostKey.line); setHostKey(null); return "Сервер добавлен в доверенные"; })}>Совпадает, доверять</button><button type="button" className="factory-link" onClick={() => setHostKey(null)}>Отмена</button></div></div>}
        {overview?.fuse.blocked && <div className="factory-error" role="alert">{overview.fuse.message} <button type="button" onClick={() => void act(async () => { await api.nodeFuseReset(); })}>Сбросить предохранитель</button></div>}
        {!overview?.fuse.blocked && (overview?.fuse.failures ?? 0) > 0 && <p>Неудачных входов подряд: {overview?.fuse.failures}. После двух приложение остановится, чтобы сервер не заблокировал ваш адрес.</p>}
      </form>

      <section className="factory-panel">
        <h2>Состояние</h2>
        {!live?.configured ? <p className="factory-empty">Сервер ещё не настроен.</p> : status == null ? <p>Нажмите «Проверить связь».</p> : <>
          <p>{status.online ? "● На связи" : `○ Нет связи${status.error ? `: ${status.error}` : ""}`}</p>
          {sys && <dl className="disk-usage">
            <div><dt>Нагрузка</dt><dd>{sys.load.map(n => n.toLocaleString("ru-RU")).join(" · ")}</dd></div>
            <div><dt>Свободно памяти</dt><dd>{mb(sys.mem_available_mb)}</dd></div>
            <div><dt>Давление CPU / память / диск</dt><dd>{pct(sys.psi.cpu)} · {pct(sys.psi.memory)} · {pct(sys.psi.io)}</dd></div>
            <div><dt>Свободно на диске</dt><dd>{mb(sys.disk_free_mb)}</dd></div>
          </dl>}
          {status.remote && Object.keys(status.remote.stages).length > 0 && <p>На сервере: {Object.entries(status.remote.stages).map(([k, v]) => `${STAGE[k] ?? k} — ${v}`).join(", ")}</p>}
        </>}
      </section>
    </div>

    <section className="factory-panel">
      <div className="factory-section-heading"><h2>Что можно передать</h2>
        <div className="factory-actions">
          <button className="factory-secondary" disabled={busy || !chosen.length || !overview?.enabled} onClick={() => setConfirm("send")}>Передать на сервер… ({chosen.length})</button>
          <button className="factory-secondary" disabled={busy || !overview?.enabled || !movable.length} onClick={() => { setPicked(new Set(movable.map(i => i.id))); setConfirm("handover"); }}>Передать и выключить ПК</button>
        </div>
      </div>
      {confirm && <div className="disk-confirm" role="alert">
        <p>{confirm === "send" ? "Передать на сервер выбранные задачи:" : "Передать на сервер все, что можно, а затем выключить ПК:"}</p>
        <ul>{items.filter(i => picked.has(i.id)).map(i => <li key={i.id}><label className="factory-check"><input type="checkbox" checked disabled={confirm === "send" ? false : false} onChange={() => toggle(i.id)} />{i.title || i.source}</label></li>)}</ul>
        {confirm === "handover" && items.some(i => !i.canMove && i.ownerNode === "local") && <p>Остаются на ПК и будут ждать его включения: {items.filter(i => !i.canMove && i.ownerNode === "local").map(i => i.title || i.source).join(", ")}.</p>}
        <p>Сервер получит ссылку или файл, шаблон, баннеры и геймплей. Публикация в соцсети с сервера не выполняется: ролики вернутся на ПК.</p>
        <div className="factory-actions">
          <button className="factory-secondary" disabled={busy || !chosen.length} onClick={() => { const mode = confirm; setConfirm(null); void act(async () => {
            if (mode === "send") { const r = await api.nodeSend(chosen); return `Передано: ${r.sent.length}${r.failed.length ? `. Не удалось: ${r.failed.map(f => `#${f[0]} — ${f[1]}`).join("; ")}` : ""}`; }
            const r = await api.nodeHandover(chosen);
            if (r.safeToPowerOff) { startCountdown(); return `Передано: ${r.sent.length}. Сервер подтвердил приём.`; }
            return `ПК не выключаю. ${r.failed.length ? `Не передано: ${r.failed.map(f => `#${f[0]} — ${f[1]}`).join("; ")}. ` : ""}${r.staysHere.length ? `На ПК ещё идёт работа: ${r.staysHere.length} шт.` : ""}`;
          }); }}>{confirm === "send" ? "Передать" : "Передать и выключить ПК"}</button>
          <button className="factory-link" onClick={() => setConfirm(null)}>Отмена</button>
        </div>
      </div>}
      {items.length === 0 ? <p className="factory-empty">В очереди пусто.</p> : <ul className="node-items">{items.map(i => <li key={i.id}>
        <label className="factory-check"><input type="checkbox" disabled={!i.canMove} checked={picked.has(i.id)} onChange={() => toggle(i.id)} /><strong>{i.title || i.source}</strong></label>
        <small>{STAGE[i.stage] ?? i.stage}{i.ownerNode !== "local" ? ` · на узле «${i.ownerNode}»` : ""}{!i.canMove && i.reason ? ` · ${i.reason}` : ""}{progress[i.id] != null && progress[i.id] < 1 ? ` · ${Math.round(progress[i.id] * 100)} %` : ""}</small>
        {i.ownerNode !== "local" && <button className="factory-link" disabled={busy} onClick={() => void act(async () => { await api.nodeRecall(i.id); return "Задача вернулась на ПК"; })}>Вернуть на ПК</button>}
      </li>)}</ul>}
      {away.length > 0 && <p>Передано на сервер: {away.length}. Готовые ролики приходят в «Готовые ролики» (файлы лежат в папке from-server рядом с данными приложения).</p>}
    </section>

    <p className="analytics-notice">Ключи Twitch, YouTube и Kick, а также уведомления в Telegram настраиваются на экране «Интеграции»: оттуда их можно отправить и на сервер.</p>
  </div>;
}
