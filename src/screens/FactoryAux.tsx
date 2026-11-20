import { useEffect, useState } from "react";
import { api, errText, type FactorySettings } from "../lib/api";
import { FactoryHeading, Field, LoadError, useFactoryData } from "../components/FactoryControls";
import VirtualRows from "../components/VirtualRows";
import { VkMetricsAccounts } from "../components/VkMetrics";
import InstagramAccounts from "../components/InstagramAccounts";
import TelegramAccounts from "../components/TelegramAccounts";
import YouTubeAccounts from "../components/YouTubeAccounts";
import { OutputProfiles } from "../components/OutputProfiles";
import TikTokCalendarPolicy from "../components/TikTokCalendarPolicy";
import TikTokPanel from "../components/TikTokPanel";
import Diagnostics from "../components/Diagnostics";
import LifecycleSettings from "../components/LifecycleSettings";
const readSettings = api.factorySettings;
const readNotifications = api.notificationsEnabled;
const readPower = api.powerStatus;
function PowerStatus() {
  const { data, error, refresh } = useFactoryData(readPower);
  useEffect(() => { const t = window.setInterval(() => void refresh(), 3000); return () => window.clearInterval(t); }, [refresh]);
  return <div className="factory-panel" aria-label="Состояние блокировки сна"><h2>Защита от сна</h2><LoadError error={error} retry={refresh} /><p role="status">{data ? `${data.active ? "Сон заблокирован" : data.needed ? "Блокировка недоступна" : "Сон разрешён"}. ${data.reason}` : "Проверяю…"}</p>{data?.error && <p className="factory-error">{data.error}</p>}<p>Мониторинг и ожидание назначенного слота удерживают компьютер включённым. Во время простоя блокировка снимается автоматически.</p></div>;
}
function NotificationSettings() {
  const { data, error, refresh } = useFactoryData(readNotifications); const [message, setMessage] = useState(""); const [busy, setBusy] = useState(false);
  return <section className="factory-panel"><h2>Уведомления рабочего стола</h2><p>Сбой фабрики, остановка публикаций, капча и исчерпанная квота. Повтор одного предупреждения ограничен 15 минутами. Все причины остаются в журнале.</p><LoadError error={error} retry={refresh} /><label className="factory-check"><input type="checkbox" checked={data ?? false} disabled={data == null || busy} onChange={async e => { setBusy(true); try { await api.notificationsSet(e.target.checked); await refresh(); setMessage("Настройка сохранена"); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } }} />Показывать уведомления</label><span role="status">{message}</span></section>;
}
export function Settings() {
  const { data, error, refresh } = useFactoryData(readSettings); const [settings, setSettings] = useState<FactorySettings | null>(null); const [busy, setBusy] = useState(false); const [message, setMessage] = useState("");
  useEffect(() => { if (data) setSettings(data); }, [data]);
  return <div className="factory-page"><FactoryHeading title="Настройки" detail="Ресурсы локальной фабрики и восстановление работы." /><LoadError error={error} retry={refresh} />{settings && <form className="factory-panel factory-form max-w-[760px]" onSubmit={async e => { e.preventDefault(); setBusy(true); try { setSettings(await api.factorySettingsSet(settings)); setMessage("Настройки сохранены"); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } }}>
    <h2>Параллельная обработка</h2><div className="factory-fields"><Field label="Черновиков одновременно"><input className="factory-input" type="number" min={1} max={4} value={settings.draftConcurrency} onChange={e => setSettings({ ...settings, draftConcurrency: +e.target.value })} /></Field><Field label="Финалов одновременно"><input className="factory-input" type="number" min={1} max={2} value={settings.finalConcurrency} onChange={e => setSettings({ ...settings, finalConcurrency: +e.target.value })} /></Field></div>
    <p>Новый лимит действует на следующие задачи. Активные рендеры завершаются. При 4 ГБ видеопамяти рекомендуется один финал.</p><div className="factory-resource-row"><span>Whisper + LLM</span><strong>1 общая дорожка</strong><span>Загрузки</span><strong>2 одновременно</strong></div>
    <label className="factory-check"><input type="checkbox" checked={settings.resumeOnStart} onChange={e => setSettings({ ...settings, resumeOnStart: e.target.checked })} />Продолжать при запуске приложения</label><p>Возобновляются обработка, мониторинг и ранее запущенные публикации. Сохранённая пауза, ошибка и отправка, требующая проверки, ждут твоего действия.</p><label className="factory-check"><input type="checkbox" checked={settings.inhibitSleep ?? true} onChange={e => setSettings({ ...settings, inhibitSleep: e.target.checked })} />Блокировать сон во время работы фабрики</label><div className="factory-actions"><button className="factory-primary" disabled={busy}>{busy ? "Сохраняю…" : "Сохранить настройки"}</button><span role="status">{message}</span></div>
  </form>}<PowerStatus /><LifecycleSettings /><NotificationSettings /><Diagnostics /><OutputProfiles /></div>;
}
function AccountSetup() {
  const open = (id: string) => {
    const heading = document.getElementById(id);
    heading?.scrollIntoView({ block: "start", behavior: "instant" });
    heading?.focus({ preventScroll: true });
  };
  return <section className="factory-panel account-setup" aria-labelledby="account-setup-title">
    <h2 id="account-setup-title">Подключение площадок</h2>
    <p>Telegram и VK можно настроить в любой момент. До подключения доступны анализ, монтаж и подготовка роликов.</p>
    <div className="account-setup-row"><div><h3>Telegram</h3><p>Бот и канал для публикаций по календарю.</p></div><button type="button" className="factory-secondary" onClick={() => open("telegram-account-settings")}>Настроить Telegram</button></div>
    <div className="account-setup-row"><div><h3>VK</h3><p>Статистика уже опубликованных роликов. Автозагрузка клипов пока недоступна.</p></div><button type="button" className="factory-secondary" onClick={() => open("vk-account-settings")}>Настроить VK</button></div>
  </section>;
}
const readAccounts = api.accountsList;
export function Accounts() {
  const { data, error, refresh } = useFactoryData(readAccounts); const [names, setNames] = useState(""); const [busy, setBusy] = useState(false); const [message, setMessage] = useState("");
  const act = async (fn: () => Promise<unknown>) => { setBusy(true); setMessage(""); try { await fn(); await refresh(); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } };
  return <div className="factory-page"><FactoryHeading title="Аккаунты" detail="Аккаунты для публикаций и подключение статистики VK." /><AccountSetup /><LoadError error={error} retry={refresh} />{message && <p className="factory-error" role="alert">{message}</p>}<section className="factory-panel"><h2>TikTok · {data?.length ?? 0}</h2><VirtualRows items={data ?? []} rowHeight={70} label="Аккаунты TikTok" render={account => <div className="factory-calendar-row"><span><strong>{account.label}{account.main ? " · основной" : ""}</strong><small>Сегодня опубликовано: {account.sentToday}</small></span><label className="factory-check"><input type="checkbox" checked={account.enabled} disabled={busy} onChange={e => void act(() => api.accountsSetEnabled(account.id, e.target.checked))} />Включён</label><div className="factory-actions"><button className="factory-link" disabled={busy} onClick={() => void act(() => api.accountsOpenLogin(account.id))}>Открыть вход</button>{!account.main && <button className="factory-link" disabled={busy} onClick={() => void act(() => api.accountsRemove(account.id))}>Убрать</button>}</div></div>} /><form className="factory-actions mt-4" onSubmit={e => { e.preventDefault(); void act(async () => { await api.accountsAdd(names.split("\n").map(s => s.trim()).filter(Boolean)); setNames(""); }); }}><input aria-label="Название нового аккаунта" className="factory-input" value={names} onChange={e => setNames(e.target.value)} placeholder="Название нового аккаунта" /><button className="factory-secondary" disabled={busy || !names.trim()}>Добавить</button></form></section><TikTokCalendarPolicy key={data?.map(a => `${a.id}:${a.enabled}`).join(",")} /><TelegramAccounts /><YouTubeAccounts /><InstagramAccounts /><VkMetricsAccounts /><section className="factory-panel"><h2>Способ входа и отправки</h2><TikTokPanel /></section></div>;
}
