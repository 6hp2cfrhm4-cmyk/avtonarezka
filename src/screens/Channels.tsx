import { useState } from "react";
import { api, defaultAnalysisSettings, defaultMonitorOptions, errText, inTauri, type Channel, type ChannelTarget } from "../lib/api";
import { useUi } from "../store";
import { FactoryHeading, Field, LoadError, useFactoryData } from "../components/FactoryControls";
import VirtualRows from "../components/VirtualRows";
import ChannelRights from "../components/ChannelRights";
import CameraProfile from "../components/CameraProfile";
const read = async () => {
  const [channels, templates, accounts, campaigns] = await Promise.allSettled([api.channelsList(), api.templatesList(), api.publicationAccounts(), api.campaignsList()]);
  if (channels.status === "rejected") throw channels.reason;
  if (templates.status === "rejected") throw templates.reason;
  if (campaigns.status === "rejected") throw campaigns.reason;
  return { campaigns: campaigns.value, channels: channels.value, templates: templates.value, accounts: accounts.status === "fulfilled" ? accounts.value : [], accountsError: accounts.status === "rejected" ? errText(accounts.reason) : "" };
};
const modes = { auto: "Авто", review: "С одобрением", manual: "Вручную" };
const newChannel = (templateId: number | null): Channel => ({
  id: 0, name: "", sourceKind: "file", source: "", templateId, mode: "review", enabled: true,
  analysis: structuredClone(defaultAnalysisSettings), scoreThreshold: 0.5, maxClips: 5,
  complaintLimit: 3, rightsPaused: false, activeIncidents: 0,
  permission: "unspecified", credit: "", targets: [], campaignIds: [], camRect: null,
  monitor: structuredClone(defaultMonitorOptions), lastCheckedAt: null, nextCheckAt: null, sourceError: null,
});
const platforms: [ChannelTarget["platform"], string][] = [["tiktok", "TikTok"], ["youtube", "YouTube Shorts"], ["instagram", "Instagram Reels"], ["vk", "VK Клипы"], ["telegram", "Telegram"]];
export default function Channels() {
  const { data, error, refresh } = useFactoryData(read);
  const [editing, setEditing] = useState<Channel | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const notify = useUi(s => s.notify);
  const patch = (p: Partial<Channel>) => setEditing(c => c && ({ ...c, ...p }));
  const patchMonitor = (p: Partial<Channel["monitor"]>) => setEditing(c => c && ({ ...c, monitor: { ...c.monitor, ...p } }));
  const check = async () => {
    if (!editing) return; setBusy(true); setMessage("");
    try { const saved = await api.channelSave(editing); setEditing(saved); const result = await api.sourceCheck(saved.id); await refresh(); setEditing((await api.channelsList()).find(c => c.id === saved.id) ?? saved); notify(`Найдено ${result.found}, в очередь ${result.queued}, по фильтру ${result.skipped}, уже видели ${result.seen}`); }
    catch (e) { setMessage(errText(e)); await refresh(); } finally { setBusy(false); }
  };
  const save = async () => {
    if (!editing) return; setBusy(true); setMessage("");
    try { await api.channelSave(editing); await refresh(); setEditing(null); notify("Профиль канала сохранён"); }
    catch (e) { setMessage(errText(e)); } finally { setBusy(false); }
  };
  return <div className="factory-page">
    <FactoryHeading title="Каналы" detail="Источник, оформление и правила обработки — в одном профиле.">
      <button className="factory-primary" disabled={!data} onClick={() => { setEditing(newChannel(data?.templates[0]?.id ?? null)); setMessage(""); }}>Добавить канал</button>
    </FactoryHeading>
    <LoadError error={error} retry={refresh} />
    {!data && !error && <p>Загружаю профили…</p>}
    <div className={editing ? "factory-editor-grid" : ""}>
      <section className="factory-panel">
        <h2>Профили · {data?.channels.length ?? 0}</h2>
        {data?.channels.length === 0 && <p className="factory-empty">Добавь первый канал. По умолчанию ролики ждут одобрения.</p>}
        <VirtualRows items={data?.channels ?? []} label="Профили каналов" rowHeight={100} render={c => <button data-channel-id={c.id} className={`factory-object-row ${editing?.id === c.id ? "is-selected" : ""}`} onClick={() => { setEditing(structuredClone(c)); setMessage(""); }}>
          <span><strong>{c.name}</strong><small>{modes[c.mode]} · до {c.maxClips} роликов на источник</small><small title={c.sourceError ?? undefined}>{c.sourceError ? `Ошибка: ${c.sourceError}` : c.lastCheckedAt ? `Проверен ${new Date(c.lastCheckedAt * 1000).toLocaleString("ru-RU", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" })}` : c.monitor.enabled ? `Проверка каждые ${c.monitor.intervalMinutes} мин` : "Мониторинг выключен"}</small></span><span>{c.rightsPaused ? "Жалобы · пауза" : c.enabled ? "Включён" : "Пауза"}</span>
        </button>} />
      </section>
      {editing && <form className="factory-panel factory-form" onSubmit={e => { e.preventDefault(); void save(); }}>
        <div className="factory-section-heading"><h2>{editing.id ? "Профиль канала" : "Новый канал"}</h2><div className="factory-actions"><button className="factory-primary" disabled={busy}>Сохранить</button><button type="button" className="factory-link" onClick={() => setEditing(null)}>Закрыть</button></div></div>
        <Field label="Название"><input className="factory-input" autoFocus required value={editing.name} onChange={e => patch({ name: e.target.value })} /></Field>
        <div className="factory-fields">
          <Field label="Источник"><select className="factory-input" value={editing.sourceKind} onChange={e => patch({ sourceKind: e.target.value as Channel["sourceKind"] })}><option value="file">Локальный файл</option><option value="youtube">YouTube</option><option value="folder">Папка с видео</option><option value="twitch">Twitch</option><option value="kick">Kick</option></select></Field>
          <Field label="Режим"><select className="factory-input" value={editing.mode} onChange={e => patch({ mode: e.target.value as Channel["mode"] })}>{Object.entries(modes).map(([v, label]) => <option key={v} value={v}>{label}</option>)}</select></Field>
        </div>
        <Field label="Адрес источника" hint={editing.sourceKind === "folder" ? "Файлы в корне папки, без изменений не менее 30 секунд. Временные и незавершённые файлы пропускаются." : editing.sourceKind === "file" ? "Абсолютный путь. Повторная проверка не добавляет тот же файл заново." : "Запись, канал, плейлист YouTube или вкладка /streams. При опросе проверяются последние 200 записей."}><input className="factory-input" value={editing.source} placeholder={editing.sourceKind === "folder" ? "/путь/к/видео" : "Путь к файлу или ссылка"} onChange={e => patch({ source: e.target.value })} /></Field>
        <details open className="factory-details"><summary>Мониторинг источника</summary><div className="factory-form">
          <label className="factory-check"><input type="checkbox" checked={editing.monitor.enabled} onChange={e => patchMonitor({ enabled: e.target.checked })} />Находить новые записи, пока приложение открыто</label>
          <div className="factory-fields">
            <Field label="Проверять каждые, мин"><input className="factory-input" type="number" min={1} max={1440} required value={editing.monitor.intervalMinutes} onChange={e => patchMonitor({ intervalMinutes: +e.target.value })} /></Field>
            <Field label="Мин. длительность, с"><input className="factory-input" type="number" min={0} value={editing.monitor.minDuration ?? ""} onChange={e => patchMonitor({ minDuration: e.target.value === "" ? null : +e.target.value })} placeholder="Без ограничения" /></Field>
            <Field label="Макс. длительность, с"><input className="factory-input" type="number" min={0} value={editing.monitor.maxDuration ?? ""} onChange={e => patchMonitor({ maxDuration: e.target.value === "" ? null : +e.target.value })} placeholder="Без ограничения" /></Field>
          </div>
          <Field label="Фильтр названия (regex)" hint="Например, (?i)стрим|обзор. Пустое поле пропускает любое название."><input className="factory-input" value={editing.monitor.titleRegex} onChange={e => patchMonitor({ titleRegex: e.target.value })} /></Field>
          {!["file", "folder"].includes(editing.sourceKind) && <>
            <label className="factory-check"><input type="checkbox" checked={editing.monitor.twoPass} onChange={e => patchMonitor({ twoPass: e.target.checked })} />Анализировать прокси, загружать выбранные фрагменты в 1080p</label>
            <label className="factory-check"><input type="checkbox" checked={editing.monitor.downloadChat} onChange={e => patchMonitor({ downloadChat: e.target.checked })} />Получать replay-чат для отбора моментов</label>
            <p>YouTube использует yt-dlp, Twitch требует установленный TwitchDownloaderCLI. Kick пока поддерживает импорт JSON на экране анализа.</p>
            {editing.sourceKind === "twitch" && <label className="factory-check"><input type="checkbox" checked={editing.monitor.viewerClips} onChange={e => patchMonitor({ viewerClips: e.target.checked })} />Учитывать клипы зрителей через Twitch API</label>}
            <p>При недоступности секций загрузится полный исходник. Режим «Вручную» сразу получает полный файл для свободного выбора границ.</p>
            <Field label="Cookies из моего браузера" hint="Только для записей, доступных тебе в этом браузере. По умолчанию cookies не читаются."><select className="factory-input" value={editing.monitor.cookiesBrowser ?? ""} onChange={e => patchMonitor({ cookiesBrowser: e.target.value || null })}><option value="">Не использовать</option>{["chromium", "chrome", "firefox", "edge", "brave", "opera", "vivaldi", "safari"].map(b => <option key={b} value={b}>{b}</option>)}</select></Field>
            {editing.sourceKind === "kick" && <p>Публичный архив и скачивание секций Kick проверены. Доступность записи зависит от площадки и установленной версии yt-dlp.</p>}
          </>}
          {editing.sourceError && <div className="factory-error" role="status">{editing.sourceError}</div>}
          <div className="factory-actions"><button type="button" className="factory-secondary" disabled={busy || !editing.source.trim() || !inTauri || !editing.enabled || editing.rightsPaused} onClick={() => void check()}>{busy ? "Проверяю…" : "Сохранить и проверить сейчас"}</button><span>{editing.nextCheckAt && editing.monitor.enabled ? `Следующая проверка: ${new Date(editing.nextCheckAt * 1000).toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" })}` : ""}</span></div>
          {!inTauri && <p>Опрос источников работает в настольном приложении.</p>}
        </div></details>
        <Field label="Шаблон оформления"><select className="factory-input" value={editing.templateId ?? ""} onChange={e => patch({ templateId: +e.target.value || null })}><option value="">По умолчанию</option>{data?.templates.map(t => <option key={t.id} value={t.id}>{t.name}</option>)}</select></Field>
        <CameraProfile key={editing.id} value={editing.camRect} source={editing.source} onChange={camRect => patch({ camRect })} />
        <div className="factory-fields">
          <Field label="Максимум роликов на источник"><input className="factory-input" type="number" min={1} max={60} required value={editing.maxClips} onChange={e => patch({ maxClips: +e.target.value })} /></Field>
          <Field label="Порог оценки для auto"><input className="factory-input" type="number" min={0} max={1} step={0.05} required value={editing.scoreThreshold} onChange={e => patch({ scoreThreshold: +e.target.value })} /></Field>
        </div>
        <details open className="factory-details"><summary>Анализ и локальный ИИ</summary><div className="factory-form">
          <div className="factory-fields">
            <Field label="Нарезка"><select className="factory-input" value={editing.analysis.fixedSeconds === null ? "best" : "fixed"} onChange={e => patch({ analysis: { ...editing.analysis, fixedSeconds: e.target.value === "fixed" ? 30 : null } })}><option value="best">Лучшие моменты</option><option value="fixed">По N секунд</option></select></Field>
            <Field label="Язык Whisper"><select className="factory-input" value={editing.analysis.language} onChange={e => patch({ analysis: { ...editing.analysis, language: e.target.value } })}><option value="ru">Русский</option><option value="auto">Авто</option><option value="en">Английский</option><option value="uk">Украинский</option><option value="es">Испанский</option></select></Field>
          </div>
          {editing.analysis.fixedSeconds !== null && <Field label="Длина фрагмента, секунд" hint="Сплошные части берутся по времени до установленного максимума; порог оценки не применяется."><input className="factory-input" type="number" min={5} max={180} value={editing.analysis.fixedSeconds} onChange={e => patch({ analysis: { ...editing.analysis, fixedSeconds: +e.target.value } })} /></Field>}
          <div className="factory-checks">{([["useEvents", "События YAMNet"], ["useSpeech", "Речь Whisper"], ["useLlm", "Оценка LLM"]] as const).map(([key, label]) => <label key={key}><input type="checkbox" checked={editing.analysis[key]} onChange={e => patch({ analysis: { ...editing.analysis, [key]: e.target.checked } })} />{label}</label>)}</div>
          <label className="factory-check"><input type="checkbox" checked={editing.analysis.scoutEnabled} onChange={e => patch({ analysis: { ...editing.analysis, scoutEnabled: e.target.checked } })} />Распознавать только горячие окна длинной записи</label>
          <Field label="Доля записи для Whisper, %" hint="Скаут работает на записях длиннее 10 минут. Сплошная нарезка всегда распознаётся целиком."><input className="factory-input" type="number" min={10} max={75} step={5} value={Math.round(editing.analysis.scoutCoverage * 100)} onChange={e => patch({ analysis: { ...editing.analysis, scoutCoverage: +e.target.value / 100 } })} /></Field>
          <details><summary>Расширенные параметры</summary><div className="factory-fields">{([["minLen", "Минимум, с", 5, 170], ["maxLen", "Максимум, с", 10, 180], ["topK", "Кандидатов", 1, 60], ["llmTop", "Оценить LLM", 1, 60], ["llmWeight", "Вес LLM", 0, 1]] as const).map(([key, label, min, max]) => <Field key={key} label={label}><input className="factory-input" type="number" step={max === 1 ? 0.05 : 1} min={min} max={max} value={editing.analysis[key]} onChange={e => patch({ analysis: { ...editing.analysis, [key]: +e.target.value } })} /></Field>)}
            {Object.entries(editing.analysis.weights).map(([key, value]) => <Field key={key} label={`Вес: ${{ loudness: "громкость", motion: "движение", events: "события", speech: "речь", chat: "чат", viewer_clips: "клипы зрителей", camera: "активность камеры" }[key]}`}><input className="factory-input" type="number" min={0} max={1} step="any" value={+value.toFixed(6)} onChange={e => patch({ analysis: { ...editing.analysis, weights: { ...editing.analysis.weights, [key]: +e.target.value } } })} /></Field>)}
          </div></details>
        </div></details>
        <details className="factory-details"><summary>Рекламные кампании · {editing.campaignIds.length}</summary><div className="factory-form"><p>Баннер закрепляется при создании черновика. Новые правила действуют на следующие ролики.</p><div className="factory-checks">{data?.campaigns.map(c => <label key={c.id}><input type="checkbox" checked={editing.campaignIds.includes(c.id)} onChange={e => patch({ campaignIds: e.target.checked ? [...editing.campaignIds, c.id] : editing.campaignIds.filter(id => id !== c.id) })} />{c.name}{!c.enabled && " · выключена"}</label>)}</div>{!data?.campaigns.length && <p>Создай кампанию в разделе «Кампании».</p>}</div></details>
        <details open={editing.rightsPaused || undefined} className="factory-details"><summary>Права, кредит и цели публикации</summary><div className="factory-form">
          <Field label="Разрешение на контент"><select className="factory-input" value={editing.permission} onChange={e => patch({ permission: e.target.value as Channel["permission"] })}><option value="unspecified">Не указано</option><option value="owned">Мой контент</option><option value="agreed">По договорённости</option></select></Field>
          <Field label="Кредит источника"><input className="factory-input" value={editing.credit} placeholder="Источник: …" onChange={e => patch({ credit: e.target.value })} /></Field>
          <Field label="Автопауза после событий" hint="Считаются неразобранные жалобы и ограничения. Пауза сохраняется до ручного возобновления."><input className="factory-input" type="number" min={1} max={100} required value={editing.complaintLimit} onChange={e => patch({ complaintLimit: +e.target.value })} /></Field>
          {editing.id > 0 && <ChannelRights key={editing.id} channel={editing} onChanged={async () => { const current = (await api.channelsList()).find(c => c.id === editing.id); if (current) patch({ rightsPaused: current.rightsPaused, activeIncidents: current.activeIncidents }); await refresh(); }} />}
          <p>Готовые ролики займут ближайшие слоты включённых аккаунтов выбранных площадок. Подключи аккаунты перед автозаполнением календаря.</p>
          <LoadError error={data?.accountsError ? `Не удалось прочитать аккаунты: ${data.accountsError}` : ""} retry={refresh} />
          <div className="factory-checks">{platforms.map(([platform, label]) => <label key={platform}><input type="checkbox" checked={editing.targets.some(t => t.platform === platform)} onChange={e => patch({ targets: e.target.checked ? [...editing.targets, { platform, accountId: null }] : editing.targets.filter(t => t.platform !== platform) })} />{label}</label>)}</div>
          {platforms.filter(([platform]) => editing.targets.some(t => t.platform === platform)).map(([platform, label]) => <fieldset className="factory-form border-0 p-0 m-0 min-w-0" key={platform}><legend>{label} · аккаунты публикации</legend><label className="factory-check"><input type="radio" name={`target-mode-${platform}`} checked={!editing.targets.some(t => t.platform === platform && t.accountId !== null)} onChange={e => { if (e.target.checked) patch({ targets: [...editing.targets.filter(t => t.platform !== platform), { platform, accountId: null }] }); }} />Ротация включённых аккаунтов: один аккаунт на ролик</label><div className="factory-checks">{data?.accounts.filter(a => a.platform === platform && platform !== "vk").map(a => <label key={a.id}><input type="checkbox" checked={editing.targets.some(t => t.platform === platform && t.accountId === a.id)} onChange={e => { const other = editing.targets.filter(t => t.platform !== platform); const chosen = editing.targets.filter(t => t.platform === platform && t.accountId !== null && t.accountId !== a.id); if (e.target.checked) chosen.push({ platform, accountId: a.id }); patch({ targets: [...other, ...(chosen.length ? chosen : [{ platform, accountId: null }])] }); }} />{a.label}{a.enabled ? "" : " · пауза"}</label>)}</div>{platform === "vk" ? <p>Автопубликация VK пока недоступна. В «Аккаунтах» подключается только статистика уже опубликованных роликов.</p> : !data?.accounts.some(a => a.platform === platform) && <p>Добавь аккаунт {label} в разделе «Аккаунты».</p>}</fieldset>)}
        </div></details>
        <label className="factory-check"><input type="checkbox" checked={editing.enabled} onChange={e => patch({ enabled: e.target.checked })} />Канал включён</label>
        {message && <div className="factory-error" role="alert">{message}</div>}
        <div className="factory-actions"><button className="factory-primary" disabled={busy}>{busy ? "Сохраняю…" : "Сохранить профиль"}</button>{editing.id > 0 && <button type="button" className="factory-link" disabled={busy} onClick={async () => { setBusy(true); try { await api.channelRemove(editing.id); setEditing(null); await refresh(); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } }}>Удалить профиль</button>}</div>
      </form>}
    </div>
  </div>;
}
