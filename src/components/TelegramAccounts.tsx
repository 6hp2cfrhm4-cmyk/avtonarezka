import { useState } from "react";
import { api, errText, inTauri, type TelegramAccountArgs, type TelegramConnectionCheck } from "../lib/api";
import { Field, LoadError, useFactoryData } from "./FactoryControls";
const read = api.publicationAccounts;
const blank = (): TelegramAccountArgs => ({ id: null, label: "", chat: "", token: "", enabled: true, policy: { pauseMin: 45, dailyLimit: 10, windowStart: 0, windowEnd: 1440 } });
const clock = (minutes: number) => `${Math.floor(minutes / 60) % 24}`.padStart(2, "0") + ":" + `${minutes % 60}`.padStart(2, "0");
const minute = (value: string) => { const [h, m] = value.split(":").map(Number); return h * 60 + m; };
export default function TelegramAccounts() {
  const { data, error, refresh } = useFactoryData(read);
  const [args, setValue] = useState(blank); const [busy, setBusy] = useState(false); const [checking, setChecking] = useState(false); const [message, setMessage] = useState("");
  const [checkError, setCheckError] = useState("");
  const [check, setCheck] = useState<TelegramConnectionCheck | null>(null);
  const setArgs = (next: TelegramAccountArgs) => { setValue(next); setCheck(null); setCheckError(""); setMessage(""); };
  return <section className="factory-panel factory-form" aria-labelledby="telegram-account-settings"><div className="factory-section-heading"><h2 id="telegram-account-settings" className="account-setup-target" tabIndex={-1}>Telegram · {data?.filter(a => a.platform === "telegram").length ?? 0}</h2><button className="factory-link" disabled={busy} onClick={() => { setArgs(blank()); setMessage(""); }}>Новый канал</button></div>
    <p>Бот должен быть администратором канала с правом публикации. Токен хранится в системной связке ключей; если она недоступна, потребуется ввести его после перезапуска.</p><details className="account-setup-help"><summary>Как подключить Telegram</summary><ol>
      <li>В Telegram открой @BotFather, отправь /newbot и сохрани полученный токен.</li>
      <li>Добавь бота в свой канал администратором и разреши публикацию сообщений.</li>
      <li>Заполни название, @имя канала или числовой chat_id и токен ниже. Для закрытого канала нужен числовой ID.</li>
      <li>Выбери паузу, лимит и время публикаций, затем нажми «Сохранить Telegram». Для подготовки без отправки сними флажок «Канал включён для публикаций».</li>
    </ol><p>При желании нажми «Проверить доступ Telegram» перед сохранением: проверка покажет бота и канал, подтвердит право публикации, не отправляя сообщений. Само сохранение не проверяет права. После подключения выбери канал в целях профиля на экране «Каналы», а готовые ролики назначь в «Календаре».</p></details><LoadError error={error} retry={refresh} />
    <div className="factory-actions">{data?.filter(a => a.platform === "telegram").map(a => <button className="factory-secondary" key={a.id} data-telegram-account={a.id} disabled={busy} aria-pressed={args.id === a.id} onClick={() => { setArgs({ id: a.id, label: a.label, chat: a.externalRef, token: "", enabled: a.enabled, policy: a.policy }); setMessage(""); }}>{a.label} · {a.enabled ? "включён" : "пауза"}{a.hasSecret ? "" : " · нет токена"}</button>)}</div>
    <form className="factory-form" onSubmit={async e => { e.preventDefault(); setBusy(true); setMessage(""); try { const id = await api.telegramAccountSave({ ...args, token: args.token || null }); setArgs({ ...args, id, token: "" }); setMessage("Канал сохранён. Публикация запускается из календаря."); await refresh(); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } }}>
      <fieldset className="factory-form border-0 p-0 m-0 min-w-0" disabled={busy}>
        <div className="factory-fields"><Field label="Название канала Telegram"><input required maxLength={60} className="factory-input" value={args.label} onChange={e => setArgs({ ...args, label: e.target.value })} /></Field><Field label="Канал Telegram" hint="@имя_канала или числовой chat_id"><input required className="factory-input" value={args.chat} onChange={e => setArgs({ ...args, chat: e.target.value })} /></Field></div>
        <Field label="Токен бота Telegram" hint={args.id ? "Оставь пустым, чтобы сохранить действующий токен." : "Токен из BotFather. В SQLite он не записывается."}><input type="password" autoComplete="new-password" required={!args.id} className="factory-input" value={args.token ?? ""} onChange={e => setArgs({ ...args, token: e.target.value })} /></Field>
        <div className="factory-actions"><button type="button" className="factory-secondary" disabled={!inTauri || busy || !args.chat.trim() || (!args.id && !args.token?.trim())} onClick={async () => { setBusy(true); setChecking(true); setCheck(null); setCheckError(""); setMessage(""); try { setCheck(await api.telegramAccountCheck({ id: args.id, chat: args.chat, token: args.token || null })); } catch (e) { setCheckError(errText(e)); } finally { setChecking(false); setBusy(false); } }}>{checking ? "Проверяю доступ…" : "Проверить доступ Telegram"}</button><span>Проверяет бота и право публикации без отправки сообщений.</span></div>
        {checkError && <p role="alert">{checkError}</p>}
        {check && <p role="status" data-telegram-check>Доступ проверен: @{check.botUsername} → {check.chatTitle || "канал"} ({check.chatId}). Бот может публиковать сообщения. Проверено {new Date(check.checkedAt * 1000).toLocaleString("ru-RU")}. Проверку можно повторить перед публикацией.</p>}
        <div className="factory-fields"><Field label="Пауза Telegram, мин"><input className="factory-input" type="number" min={1} max={1440} value={args.policy.pauseMin} onChange={e => setArgs({ ...args, policy: { ...args.policy, pauseMin: +e.target.value } })} /></Field><Field label="Лимит Telegram в сутки"><input className="factory-input" type="number" min={1} max={100} value={args.policy.dailyLimit} onChange={e => setArgs({ ...args, policy: { ...args.policy, dailyLimit: +e.target.value } })} /></Field><Field label="Окно Telegram с"><input className="factory-input" type="time" value={clock(args.policy.windowStart)} onChange={e => setArgs({ ...args, policy: { ...args.policy, windowStart: minute(e.target.value) } })} /></Field><Field label="Окно Telegram до"><input className="factory-input" type="time" value={clock(args.policy.windowEnd)} onChange={e => setArgs({ ...args, policy: { ...args.policy, windowEnd: minute(e.target.value) } })} /></Field></div>
        <p>Время местное. Одинаковые границы означают весь день; окно может проходить через полночь.</p><label className="factory-check"><input type="checkbox" checked={args.enabled} onChange={e => setArgs({ ...args, enabled: e.target.checked })} />Канал включён для публикаций</label>
        <div className="factory-actions"><button className="factory-secondary" disabled={!inTauri || busy}>{busy && !checking ? "Сохраняю…" : "Сохранить Telegram"}</button><span role="status">{message}</span></div>
      </fieldset>
    </form>
  </section>;
}
