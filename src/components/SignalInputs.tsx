import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { api, errText, inTauri, type AnalysisInputs, type TwitchSignalStatus } from "../lib/api";

export default function SignalInputs({ projectId, disabled }: { projectId: number; disabled: boolean }) {
  const [inputs, setInputs] = useState<AnalysisInputs | null>(null);
  const [status, setStatus] = useState<TwitchSignalStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [vod, setVod] = useState("");
  const [clientId, setClientId] = useState("");
  const [secret, setSecret] = useState("");
  const refresh = async () => { const [i, s] = await Promise.all([api.analysisInputsGet(projectId), api.twitchSignalStatus()]); setInputs(i); setStatus(s); };
  useEffect(() => { let active = true; Promise.all([api.analysisInputsGet(projectId), api.twitchSignalStatus()]).then(([i,s]) => { if (active) { setInputs(i); setStatus(s); } }).catch(e => active && setError(errText(e))); return () => { active = false; }; }, [projectId]);
  const run = async (work: () => Promise<unknown>) => { setBusy(true); setError(""); setMessage(""); try { await work(); await refresh(); setMessage("Сигналы обновлены. Запусти анализ, чтобы пересчитать моменты."); } catch (e) { setError(errText(e)); } finally { setBusy(false); } };
  return <details className="signal-inputs"><summary>Чат и клипы зрителей</summary>
    <fieldset disabled={disabled || busy || !inTauri} className="factory-form mt-4">
      <p>Импортируй TwitchDownloader JSON, YouTube live_chat JSONL или Kick export с offset_seconds. Хранятся только показатели активности.</p>
      <div className="factory-actions"><button className="factory-secondary" onClick={() => void run(async () => { const path = await open({ multiple: false, filters: [{ name: "Чат JSON / JSONL", extensions: ["json", "jsonl"] }] }); if (typeof path === "string") setInputs(await api.analysisChatImport(projectId, path)); })}>Импортировать чат</button>
        {inputs?.chat && <button className="factory-link" onClick={() => void run(() => api.analysisChatClear(projectId))}>Убрать чат</button>}</div>
      {inputs?.chat && <p>{inputs.chat.format} · {inputs.chat.messages.toLocaleString("ru-RU")} сообщений · пропущено {inputs.chat.ignored}</p>}
      {inputs?.chatError && <p className="factory-error">{inputs.chatError}</p>}
      <div className="factory-fields"><label className="factory-field"><span>ID записи Twitch</span><input className="factory-input" inputMode="numeric" value={vod} onChange={e => setVod(e.target.value)} placeholder="Например, 1234567890" /></label><button className="factory-secondary self-end" disabled={!status?.configured || !/^\d+$/.test(vod)} onClick={() => void run(async () => setInputs(await api.analysisTwitchClips(projectId, vod)))}>Получить клипы зрителей</button></div>
      {inputs?.clips && <div><p>Клипы: {inputs.clips.clips} · без vod_offset: {inputs.clips.missingOffset}{inputs.clips.truncated ? " · показаны первые 1000, результат неполный" : ""}</p><button className="factory-link" onClick={() => void run(() => api.analysisClipsClear(projectId))}>Убрать сигнал клипов</button></div>}
      <details><summary>Twitch API · {status?.configured ? "настроен" : "нужны ключи"}</summary><div className="factory-form mt-3">
        <p>Ключи бесплатного приложения Twitch нужны для Helix Get Clips. {status?.storage === "memory" ? "Связка ключей недоступна: секрет останется в памяти до закрытия приложения." : "Секрет сохраняется в системной связке ключей."}</p>
        <label className="factory-field"><span>Client ID</span><input className="factory-input" autoComplete="off" value={clientId} onChange={e => setClientId(e.target.value)} /></label>
        <label className="factory-field"><span>Client Secret</span><input className="factory-input" autoComplete="new-password" type="password" value={secret} onChange={e => setSecret(e.target.value)} /></label>
        <div className="factory-actions"><button className="factory-secondary" disabled={!clientId.trim() || !secret.trim()} onClick={() => void run(async () => { await api.twitchSignalCredentials(clientId, secret); setSecret(""); setClientId(""); })}>Сохранить ключи</button>{status?.configured && <button className="factory-link" onClick={() => void run(() => api.twitchSignalCredentials("", ""))}>Удалить ключи</button>}</div>
      </div></details>
    </fieldset>
    {!inTauri && <p>Импорт и запросы доступны в настольном приложении.</p>}
    {error && <p className="factory-error" role="alert">{error}</p>}{busy && <p role="status">Получаю сигналы…</p>}{message && <p role="status">{message}</p>}
  </details>;
}
