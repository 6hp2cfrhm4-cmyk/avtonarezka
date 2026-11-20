import { useState } from "react";
import { api, errText, inTauri, type InstagramAccountArgs } from "../lib/api";
import { Field, LoadError, useFactoryData } from "./FactoryControls";
const read = api.publicationAccounts;
const blank = (): InstagramAccountArgs => ({ id: null, label: "", user: "", token: "", enabled: true, policy: { pauseMin: 45, dailyLimit: 10, windowStart: 0, windowEnd: 1440 } });
// taste-skill-ignore numbering: zero-padding is the actual HH:mm publication window.
const clock = (minutes: number) => `${Math.floor(minutes / 60) % 24}`.padStart(2, "0") + ":" + `${minutes % 60}`.padStart(2, "0");
const minute = (value: string) => { const [h, m] = value.split(":").map(Number); return h * 60 + m; };
export default function InstagramAccounts() {
  const { data, error, refresh } = useFactoryData(read);
  const [args, setArgs] = useState(blank); const [busy, setBusy] = useState(false); const [message, setMessage] = useState("");
  return <section className="factory-panel factory-form"><div className="factory-section-heading"><h2>Instagram · {data?.filter(a => a.platform === "instagram").length ?? 0}</h2><button className="factory-link" onClick={() => { setArgs(blank()); setMessage(""); }}>Новый аккаунт</button></div>
    <p>Instagram Business/Creator должен быть связан со страницей Facebook. В приложении Meta получи Page access token для этого аккаунта. При сохранении проверяются доступ и текущая квота. Токен хранится в связке ключей; при её недоступности потребуется повторный ввод после перезапуска.</p><LoadError error={error} retry={refresh} />
    <div className="factory-actions">{data?.filter(a => a.platform === "instagram").map(a => <button className="factory-secondary" key={a.id} aria-pressed={args.id === a.id} onClick={() => { setArgs({ id: a.id, label: a.label, user: a.externalRef, token: "", enabled: a.enabled, policy: a.policy }); setMessage(""); }}>{a.label} · {a.enabled ? "включён" : "пауза"}{a.hasSecret ? "" : " · нет токена"}</button>)}</div>
    <form className="factory-form" onSubmit={async e => { e.preventDefault(); setBusy(true); setMessage(""); try { const id = await api.instagramAccountSave({ ...args, token: args.token || null }); setArgs({ ...args, id, token: "" }); setMessage("Аккаунт и лимит проверены. Публикация запускается из календаря."); await refresh(); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } }}>
      <fieldset className="factory-form border-0 p-0 m-0 min-w-0" disabled={busy}>
        <div className="factory-fields"><Field label="Название аккаунта Instagram"><input required maxLength={60} className="factory-input" value={args.label} onChange={e => setArgs({ ...args, label: e.target.value })} /></Field><Field label="Instagram User ID" hint="Числовой ID Instagram Business/Creator"><input required className="factory-input" value={args.user} onChange={e => setArgs({ ...args, user: e.target.value })} /></Field></div>
        <Field label="Токен Facebook Page" hint={args.id ? "Оставь пустым, чтобы сохранить действующий токен." : "Нужны instagram_basic, instagram_content_publish и pages_read_engagement."}><input type="password" autoComplete="new-password" required={!args.id} className="factory-input" value={args.token ?? ""} onChange={e => setArgs({ ...args, token: e.target.value })} /></Field>
        <div className="factory-fields"><Field label="Пауза Instagram, мин"><input className="factory-input" type="number" min={1} max={1440} value={args.policy.pauseMin} onChange={e => setArgs({ ...args, policy: { ...args.policy, pauseMin: +e.target.value } })} /></Field><Field label="Лимит Instagram в сутки"><input className="factory-input" type="number" min={1} max={100} value={args.policy.dailyLimit} onChange={e => setArgs({ ...args, policy: { ...args.policy, dailyLimit: +e.target.value } })} /></Field><Field label="Окно Instagram с"><input className="factory-input" type="time" value={clock(args.policy.windowStart)} onChange={e => setArgs({ ...args, policy: { ...args.policy, windowStart: minute(e.target.value) } })} /></Field><Field label="Окно Instagram до"><input className="factory-input" type="time" value={clock(args.policy.windowEnd)} onChange={e => setArgs({ ...args, policy: { ...args.policy, windowEnd: minute(e.target.value) } })} /></Field></div>
        <p>Время местное. Одинаковые границы означают весь день; окно может проходить через полночь.</p><label className="factory-check"><input type="checkbox" checked={args.enabled} onChange={e => setArgs({ ...args, enabled: e.target.checked })} />Аккаунт включён для публикаций</label>
        <div className="factory-actions"><button className="factory-secondary" disabled={!inTauri || busy}>{busy ? "Сохраняю…" : "Сохранить Instagram"}</button><span role="status">{message}</span></div>
      </fieldset>
    </form>
  </section>;
}
