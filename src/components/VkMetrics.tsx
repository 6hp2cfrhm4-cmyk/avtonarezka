import { useState } from "react";
import { api, errText, inTauri, type VkMetricsAccountArgs, type VkMetricsClip } from "../lib/api";
import { useUi } from "../store";
import { Field, LoadError, useFactoryData } from "./FactoryControls";
import VirtualRows from "./VirtualRows";

const read = api.vkMetricsOptions;
const blank = (): VkMetricsAccountArgs => ({ id: null, label: "", video: "", token: "", enabled: true });
const clipLabel = (c: VkMetricsClip) => `${c.draftId == null ? "Готовый файл" : "Финал"} #${c.draftId ?? c.readyId}`;
const clipKey = (c: VkMetricsClip) => c.draftId == null ? `r${c.readyId}` : `d${c.draftId}`;

export function VkMetricsAccounts() {
  const { data, error, refresh } = useFactoryData(read);
  const [args, setArgs] = useState(blank); const [busy, setBusy] = useState(false); const [message, setMessage] = useState("");
  return <section className="factory-panel factory-form vk-metrics" aria-label="Подключение статистики VK">
    <div className="factory-section-heading"><h2 id="vk-account-settings" className="account-setup-target" tabIndex={-1}>VK · статистика</h2><button className="factory-link" disabled={busy} onClick={() => { setArgs(blank()); setMessage(""); }}>Новый профиль VK</button></div>
    <p>Пользовательский токен VK с доступом к видео нужен для чтения счётчиков через video.get. Проверка по ролику подтверждает доступ к его статистике. Загрузка в VK пока недоступна.</p>
    <details className="account-setup-help"><summary>Как подключить статистику VK</summary><ol>
      <li>Подготовь пользовательский токен своего приложения VK с доступом к видео. Сервисный токен или токен сообщества для этой формы не подходит.</li>
      <li>Возьми прямую ссылку на уже опубликованный ролик нужного владельца: vk.com/clip-123_456 или vk.com/video-123_456.</li>
      <li>Введи название, ссылку и токен ниже, затем нажми «Проверить и сохранить VK». Проверка только читает статистику ролика.</li>
      <li>В «Аналитике» открой «Связать уже опубликованный ролик VK», выбери локальный финал и укажи ссылку и дату публикации.</li>
    </ol><p>Профиль используется для метрик и отчётов кампаний. Чтобы временно остановить сбор, нажми «Пауза сбора» у сохранённого профиля.</p></details>
    <LoadError error={error} retry={refresh} />
    {data?.secretBackend === "memory" && <p role="status">Связка ключей недоступна. Токен хранится только до закрытия приложения; после перезапуска введи его заново.</p>}
    <VirtualRows items={data?.accounts ?? []} rowHeight={72} label="Профили статистики VK" render={a => <div className="vk-profile-controls"><button type="button" className="vk-profile-row" data-vk-profile={a.id} aria-pressed={args.id === a.id} disabled={busy} onClick={() => { setArgs({ id: a.id, label: a.label, video: a.probeVideo, token: "", enabled: a.enabled }); setMessage(""); }}><span><strong>{a.label}</strong><small>Владелец {a.owner}</small></span><span>{a.hasSecret ? a.enabled ? "Сбор включён" : "Сбор на паузе" : "Нужен токен"}</span></button><button type="button" className="factory-link" disabled={busy || !inTauri} onClick={async () => { setBusy(true); setMessage(""); try { await api.vkMetricsAccountEnabled(a.id, !a.enabled); if (args.id === a.id) setArgs({ ...args, enabled: !a.enabled }); await refresh(); setMessage(a.enabled ? "Сбор метрик VK поставлен на паузу." : "Сбор метрик VK включён."); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } }}>{a.enabled ? "Пауза сбора" : "Включить сбор"}</button></div>} />
    <form className="factory-form" onSubmit={async e => { e.preventDefault(); setBusy(true); setMessage(""); try { const id = await api.vkMetricsAccountSave({ ...args, token: args.token || null }); setArgs({ ...args, id, token: "" }); setMessage("Доступ к статистике проверен. Свяжи опубликованный ролик с финалом в «Аналитике»."); await refresh(); } catch (e) { setMessage(errText(e)); await refresh(); } finally { setBusy(false); } }}>
      <fieldset disabled={busy} className="factory-form border-0 p-0 m-0 min-w-0">
        <div className="factory-fields"><Field label="Название профиля VK"><input required maxLength={60} className="factory-input" value={args.label} onChange={e => setArgs({ ...args, label: e.target.value })} /></Field><Field label="Проверочный ролик VK" hint="Прямая ссылка vk.com/clip-123_456 либо ID -123_456. Для сообщества ID владельца отрицательный."><input required className="factory-input" value={args.video} onChange={e => setArgs({ ...args, video: e.target.value })} /></Field></div>
        <Field label="Пользовательский токен VK" hint={args.id ? "Оставь пустым, чтобы проверить сохранённый токен. Для другого владельца создай новый профиль." : "Вставь токен из своего приложения VK. Он сохраняется в связке ключей и не попадает в ссылку."}><input required={!args.id} type="password" autoComplete="new-password" className="factory-input" value={args.token ?? ""} onChange={e => setArgs({ ...args, token: e.target.value })} /></Field>
        <label className="factory-check"><input type="checkbox" checked={args.enabled} onChange={e => setArgs({ ...args, enabled: e.target.checked })} />Включить сбор метрик VK</label>
        <div className="factory-actions"><button className="factory-secondary" disabled={!inTauri || busy}>{busy ? "Проверяю…" : "Проверить и сохранить VK"}</button><button className="factory-link" type="button" disabled={busy} onClick={() => useUi.getState().setArea("analytics")}>Открыть аналитику</button></div>
      </fieldset>
    </form>{message && <p role="status">{message}</p>}
  </section>;
}

export function VkMetricsLink({ updated }: { updated: () => void }) {
  const { data, error, refresh } = useFactoryData(read);
  const [account, setAccount] = useState(""); const [search, setSearch] = useState(""); const [clip, setClip] = useState<VkMetricsClip | null>(null);
  const [video, setVideo] = useState(""); const [at, setAt] = useState(""); const [busy, setBusy] = useState(false); const [message, setMessage] = useState("");
  const candidates = (data?.clips ?? []).filter(c => `${c.title} ${c.project}`.toLocaleLowerCase().includes(search.toLocaleLowerCase())).map(c => ({ ...c, id: clipKey(c) }));
  return <details className="factory-panel vk-metrics" data-vk-link><summary><h2 className="vk-link-heading">Связать уже опубликованный ролик VK</h2></summary><div className="factory-form">
    <p>Выбери соответствующий локальный финал, ссылку VK и фактическую дату публикации. Приложение проверит счётчики и добавит ролик в аналитику и отчёт кампании.</p><LoadError error={error} retry={refresh} />
    {data && !data.accounts.length ? <p>Сначала <button type="button" className="factory-link" onClick={() => useUi.getState().setArea("accounts")}>подключи статистику VK в «Аккаунтах»</button>.</p> : data && !data.clips.length ? <p>Подготовь финал или импортируй готовое видео в <button type="button" className="factory-link" onClick={() => useUi.getState().setArea("studio")}>Студии</button>.</p> : <form className="factory-form" onSubmit={async e => { e.preventDefault(); if (!clip) return; setBusy(true); setMessage(""); try { const id = await api.vkMetricsLink({ accountId: +account, draftId: clip.draftId, readyId: clip.readyId, video, sentAt: Math.floor(new Date(at).getTime() / 1000) }); setMessage(`Ролик VK связан. Публикация #${id}: статистика проверена.`); updated(); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } }}>
      <fieldset disabled={busy} className="factory-form border-0 p-0 m-0 min-w-0">
        <Field label="Профиль статистики VK"><select required className="factory-input" value={account} onChange={e => setAccount(e.target.value)}><option value="">Выбери владельца VK</option>{data?.accounts.map(a => <option key={a.id} value={a.id}>{a.label} · {a.owner}{a.enabled ? "" : " · сбор на паузе"}{a.hasSecret ? "" : " · нужен токен"}</option>)}</select></Field>
        <Field label="Найти локальный финал"><input className="factory-input" type="search" value={search} onChange={e => setSearch(e.target.value)} /></Field>
        <VirtualRows items={candidates} height={Math.min(candidates.length * 64, 256)} virtualize={candidates.length > 4} rowHeight={64} label="Локальные финалы для VK" render={c => <button type="button" className="vk-profile-row" data-vk-clip={c.id} aria-pressed={clip != null && clipKey(clip) === c.id} onClick={() => setClip(c)}><span><strong>{c.title}</strong><small>{clipLabel(c)} · {c.project}</small></span><span>{clip != null && clipKey(clip) === c.id ? "Выбран" : "Выбрать"}</span></button>} />
        <p className="vk-selected">{clip ? `Выбран ${clipLabel(clip)}: ${clip.title} · ${clip.project}` : "Выбери финал из списка."}</p>{!candidates.length && <p>По запросу финалов нет.</p>}
        <div className="factory-fields"><Field label="Опубликованный ролик VK" hint="Прямая публичная ссылка без параметров либо ID владельца_видео."><input required className="factory-input" value={video} onChange={e => setVideo(e.target.value)} /></Field><Field label="Дата публикации VK" hint="Фактическое местное время публикации."><input required type="datetime-local" className="factory-input" value={at} onChange={e => setAt(e.target.value)} /></Field></div>
        <div className="factory-actions"><button className="factory-secondary" disabled={!inTauri || busy || !clip || !account}>{busy ? "Проверяю статистику…" : "Проверить и связать ролик"}</button></div>
      </fieldset>
    </form>}{message && <p role="status">{message}</p>}
  </div></details>;
}
