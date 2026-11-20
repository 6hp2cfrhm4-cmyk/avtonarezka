import { useEffect, useState } from "react";
import { api, errText, inTauri, type PublicationAccount, type YoutubeSettings } from "../lib/api";
import { onEvent } from "../lib/events";
import { Field, LoadError, useFactoryData } from "./FactoryControls";
const read = async () => ({ accounts: (await api.publicationAccounts()).filter(a => a.platform === "youtube"), oauth: await api.youtubeOAuthSettings() });
const clock = (n: number) => `${Math.floor(n / 60) % 24}`.padStart(2, "0") + ":" + `${n % 60}`.padStart(2, "0");
const minute = (v: string) => { const [h, m] = v.split(":").map(Number); return h * 60 + m; };
const defaults: YoutubeSettings = { privacy: "private", madeForKids: false };
export default function YouTubeAccounts() {
  const { data, error, refresh } = useFactoryData(read);
  const [client, setClient] = useState(""); const [secret, setSecret] = useState(""); const [account, setAccount] = useState<PublicationAccount | null>(null);
  const [busy, setBusy] = useState(false); const [waiting, setWaiting] = useState(false); const [message, setMessage] = useState("");
  useEffect(() => { if (data) { setClient(data.oauth.clientId); setWaiting(data.oauth.loginRunning); } }, [data]);
  useEffect(() => onEvent<{ platform: string; ok: boolean; message: string }>("platform-login", e => { if (e.platform === "youtube") { setWaiting(false); setMessage(e.message); void refresh(); } }), [refresh]);
  const act = async (fn: () => Promise<unknown>, success: string) => { setBusy(true); setMessage(""); try { await fn(); setMessage(success); await refresh(); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } };
  const settings = account?.youtubeSettings ?? defaults;
  return <section className="factory-panel factory-form"><h2>YouTube Shorts · {data?.accounts.length ?? 0}</h2>
    <p>В Google Cloud включи YouTube Data API v3 и создай OAuth-клиент типа Desktop. Вход открывается в браузере. Аккаунт и его канал проверяются перед каждой отправкой.</p>
    <LoadError error={error} retry={refresh} />
    <form className="factory-form" onSubmit={e => { e.preventDefault(); void act(async () => { await api.youtubeOAuthSave(client, secret || null); setSecret(""); }, "OAuth-клиент сохранён"); }}><div className="factory-fields">
      <Field label="Google Client ID"><input required className="factory-input" value={client} disabled={busy || waiting} onChange={e => setClient(e.target.value)} placeholder="…apps.googleusercontent.com" /></Field>
      <Field label="Google Client Secret" hint={data?.oauth.hasSecret ? "Оставь пустым, чтобы сохранить действующий секрет." : "Хранится в связке ключей, в SQLite не записывается."}><input type="password" autoComplete="new-password" className="factory-input" value={secret} disabled={busy || waiting} onChange={e => setSecret(e.target.value)} /></Field>
    </div><div className="factory-actions"><button className="factory-secondary" disabled={!inTauri || busy || waiting}>Сохранить Google OAuth</button><button type="button" className="factory-secondary" disabled={!inTauri || busy || waiting || !data?.oauth.clientId} onClick={() => void act(async () => { await api.youtubeLoginStart(); setWaiting(true); }, "В браузере выбери канал YouTube и разреши доступ")}>Подключить YouTube</button>{waiting && <button type="button" className="factory-link" onClick={() => void api.youtubeLoginCancel()}>Отменить вход YouTube</button>}</div></form>
    {data?.oauth.secretBackend === "memory" && <p>Системная связка ключей недоступна. После закрытия приложения секрет и вход потребуется указать заново.</p>}
    {!!data?.accounts.length && <Field label="Аккаунт YouTube"><select className="factory-input" value={account?.id ?? ""} onChange={e => setAccount(data.accounts.find(a => a.id === +e.target.value) ?? null)}><option value="">Выбери подключённый канал</option>{data.accounts.map(a => <option key={a.id} value={a.id}>{a.label} · {a.hasSecret ? a.enabled ? "включён" : "пауза" : "нужен повторный вход"}</option>)}</select></Field>}
    {account && <form className="factory-form" onSubmit={e => { e.preventDefault(); void act(() => api.youtubeAccountSave(account.id, account.label, account.enabled, account.policy, settings), "Настройки YouTube сохранены"); }}><fieldset className="factory-form border-0 p-0 m-0 min-w-0" disabled={busy}>
      <div className="factory-fields"><Field label="Название аккаунта YouTube"><input required maxLength={60} className="factory-input" value={account.label} onChange={e => setAccount({ ...account, label: e.target.value })} /></Field><Field label="Видимость YouTube"><select className="factory-input" value={settings.privacy} onChange={e => setAccount({ ...account, youtubeSettings: { ...settings, privacy: e.target.value as YoutubeSettings["privacy"] } })}><option value="private">Приватное</option><option value="unlisted">По ссылке</option><option value="public">Публичное</option></select></Field></div>
      <p>По умолчанию ролик приватный. Shorts должен быть вертикальным или квадратным и длиться не больше 3 минут.</p>
      <div className="factory-fields"><Field label="Пауза YouTube, мин"><input className="factory-input" type="number" min={1} max={1440} value={account.policy.pauseMin} onChange={e => setAccount({ ...account, policy: { ...account.policy, pauseMin: +e.target.value } })} /></Field><Field label="Лимит YouTube в сутки"><input className="factory-input" type="number" min={1} max={100} value={account.policy.dailyLimit} onChange={e => setAccount({ ...account, policy: { ...account.policy, dailyLimit: +e.target.value } })} /></Field><Field label="Окно YouTube с"><input className="factory-input" type="time" value={clock(account.policy.windowStart)} onChange={e => setAccount({ ...account, policy: { ...account.policy, windowStart: minute(e.target.value) } })} /></Field><Field label="Окно YouTube до"><input className="factory-input" type="time" value={clock(account.policy.windowEnd)} onChange={e => setAccount({ ...account, policy: { ...account.policy, windowEnd: minute(e.target.value) } })} /></Field></div>
      <label className="factory-check"><input type="checkbox" checked={settings.madeForKids} onChange={e => setAccount({ ...account, youtubeSettings: { ...settings, madeForKids: e.target.checked } })} />Контент специально для детей</label><label className="factory-check"><input type="checkbox" checked={account.enabled} onChange={e => setAccount({ ...account, enabled: e.target.checked })} />Аккаунт YouTube включён</label><button className="factory-secondary self-start" disabled={busy || !inTauri}>Сохранить YouTube</button>
    </fieldset></form>}
    {message && <p className="factory-notice" role="status">{message}</p>}
  </section>;
}
