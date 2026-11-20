import { useState } from "react";
import { api, errText, inTauri, type PublicationPolicy } from "../lib/api";
import { Field, LoadError, useFactoryData } from "./FactoryControls";
const clock = (n: number) => `${Math.floor(n / 60) % 24}`.padStart(2, "0") + ":" + `${n % 60}`.padStart(2, "0");
const minutes = (s: string) => { const [h, m] = s.split(":").map(Number); return h * 60 + m; };
export default function TikTokCalendarPolicy() {
  const { data, error, refresh } = useFactoryData(api.publicationAccounts);
  const [selected, setSelected] = useState(0); const [policy, setPolicy] = useState<PublicationPolicy | null>(null);
  const [busy, setBusy] = useState(false); const [message, setMessage] = useState("");
  const accounts = data?.filter(a => a.platform === "tiktok") ?? [];
  return <section className="factory-panel factory-form"><h2>TikTok · правила календаря</h2>
    <p>Для каждого аккаунта задаются свои пауза, суточный лимит и местное окно публикаций. Вход открывается в списке TikTok выше. Старые задачи Студии используют настройки своей очереди; опубликованная история учитывается в общем календаре.</p>
    <LoadError error={error} retry={refresh} />
    <Field label="Аккаунт TikTok для правил"><select className="factory-input" value={selected} disabled={busy} onChange={e => { const id = +e.target.value; setSelected(id); setPolicy(accounts.find(a => a.id === id)?.policy ?? null); setMessage(""); }}><option value={0}>Выбери аккаунт TikTok</option>{accounts.map(a => <option value={a.id} key={a.id}>{a.label} · {a.enabled ? "включён" : "пауза"}</option>)}</select></Field>
    {policy && <form className="factory-form" onSubmit={async e => { e.preventDefault(); setBusy(true); setMessage(""); try { await api.publicationAccountPolicy(selected, policy); await refresh(); setMessage("Правила календаря сохранены"); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } }}><fieldset disabled={busy} className="factory-form border-0 p-0 m-0 min-w-0"><div className="factory-fields">
      <Field label="Пауза TikTok, мин"><input className="factory-input" type="number" required min={1} max={1440} value={policy.pauseMin} onChange={e => setPolicy({ ...policy, pauseMin: +e.target.value })} /></Field>
      <Field label="Лимит TikTok в сутки"><input className="factory-input" type="number" required min={1} max={100} value={policy.dailyLimit} onChange={e => setPolicy({ ...policy, dailyLimit: +e.target.value })} /></Field>
      <Field label="Окно TikTok с"><input className="factory-input" type="time" required value={clock(policy.windowStart)} onChange={e => setPolicy({ ...policy, windowStart: minutes(e.target.value) })} /></Field>
      <Field label="Окно TikTok до"><input className="factory-input" type="time" required value={clock(policy.windowEnd)} onChange={e => setPolicy({ ...policy, windowEnd: minutes(e.target.value) })} /></Field>
    </div><p>Одинаковые границы означают весь день; окно может проходить через полночь. Новые правила проверяются перед отправкой каждого сохранённого слота.</p><div className="factory-actions"><button className="factory-secondary" disabled={busy || !inTauri}>{busy ? "Сохраняю…" : "Сохранить правила TikTok"}</button><span role="status">{message}</span></div></fieldset></form>}
  </section>;
}
