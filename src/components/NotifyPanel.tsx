import { useCallback, useEffect, useState } from "react";
import { api, errText, type NotifyConfig } from "../lib/api";
import { Field } from "./FactoryControls";

/** Уведомления на телефон через Telegram-бота: работает и на сервере, пока ПК выключен. */
export default function NotifyPanel() {
  const [cfg, setCfg] = useState<NotifyConfig | null>(null);
  const [hasToken, setHasToken] = useState(false);
  const [token, setToken] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const load = useCallback(async () => { try { const s = await api.notifyState(); setCfg(s.config); setHasToken(s.token); } catch (e) { setMessage(errText(e)); } }, []);
  useEffect(() => { void load(); }, [load]);
  if (!cfg) return null;
  const run = async (fn: () => Promise<string | void>) => { setBusy(true); setMessage(""); try { const m = await fn(); if (m) setMessage(m); await load(); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } };
  const save = () => run(async () => { await api.notifySet(cfg, token ? token : undefined); setToken(""); return "Сохранено"; });
  return <section className="factory-panel" style={{ marginTop: 16 }}>
    <h2>Уведомления в Telegram</h2>
    <p>Бот пишет вам, когда нужна проверка или что-то сломалось, и раз в сутки присылает итог. Тексты чатов и имена зрителей не отправляются. Токен хранится в защищённом хранилище.</p>
    {message && <p className="analytics-notice" role="status">{message}</p>}
    <div className="factory-fields">
      <Field label="Токен бота" hint={hasToken ? "Сохранён. Оставьте пустым, чтобы не менять." : "Выдаёт @BotFather."}><input className="factory-input" type="password" autoComplete="off" value={token} onChange={e => setToken(e.target.value)} /></Field>
      <Field label="Куда писать" hint="Числовой chat_id (напишите боту, затем узнайте id) или @канал."><input className="factory-input" value={cfg.chat} onChange={e => setCfg({ ...cfg, chat: e.target.value })} /></Field>
      <Field label="Час ежедневной сводки" hint="24 — не присылать."><input className="factory-input" type="number" min={0} max={24} value={cfg.summaryHour} onChange={e => setCfg({ ...cfg, summaryHour: Number(e.target.value) })} /></Field>
      <div className="factory-checks"><label><input type="checkbox" checked={cfg.enabled} onChange={e => setCfg({ ...cfg, enabled: e.target.checked })} />Присылать уведомления</label><label><input type="checkbox" checked={cfg.attention} onChange={e => setCfg({ ...cfg, attention: e.target.checked })} />О проверках и ошибках</label></div>
    </div>
    <div className="factory-actions" style={{ marginTop: 12 }}>
      <button className="factory-secondary" disabled={busy} onClick={() => void save()}>Сохранить</button>
      <button className="factory-secondary" disabled={busy || !hasToken} onClick={() => void run(async () => { const r = await api.nodeConfigPush(["notify"]); return `Отправлено на сервер: ${r.applied.length} шт.`; })}>Отправить на сервер</button>
      <button className="factory-secondary" disabled={busy || !hasToken || !cfg.chat} onClick={() => void run(async () => { await api.notifyTest(); return "Тестовое сообщение отправлено"; })}>Отправить пробное</button>
    </div>
  </section>;
}
