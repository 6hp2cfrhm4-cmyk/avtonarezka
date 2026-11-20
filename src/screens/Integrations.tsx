import { useCallback, useEffect, useState } from "react";
import { api, errText, type RemoteConfig } from "../lib/api";
import { FactoryHeading, Field, LoadError, useFactoryData } from "../components/FactoryControls";
import NotifyPanel from "../components/NotifyPanel";
import { useUi } from "../store";

type Kind = "twitch" | "youtube" | "kick";

/** Карточка одного ключа: ввод, сохранение, проверка запросом, отправка на сервер. */
function KeyCard({ kind, title, hint, configured, serverHas, serverKnown, fields, onSave, onDone }: {
  kind: Kind; title: string; hint: string; configured: boolean; serverHas: boolean; serverKnown: boolean;
  fields: { id: string; label: string; secret?: boolean }[];
  onSave: (values: Record<string, string>) => Promise<void>; onDone: (m: string) => void;
}) {
  const [values, setValues] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const run = async (fn: () => Promise<string | void>) => { setBusy(true); try { const m = await fn(); if (m) onDone(m); } catch (e) { onDone(errText(e)); } finally { setBusy(false); } };
  const filled = fields.every(f => (values[f.id] ?? "").trim());
  return <section className="factory-panel">
    <div className="factory-section-heading"><h2>{title}</h2><span className="trend-chip">{configured ? "ключ сохранён" : "не настроено"}</span></div>
    <p>{hint}</p>
    <div className="factory-form" style={{ display: "grid", gap: 10 }}>
      {fields.map(f => <Field key={f.id} label={f.label}><input className="factory-input" type={f.secret ? "password" : "text"} autoComplete="off" value={values[f.id] ?? ""} onChange={e => setValues({ ...values, [f.id]: e.target.value })} /></Field>)}
      <div className="factory-actions">
        <button className="factory-secondary" disabled={busy || !filled} onClick={() => void run(async () => { await onSave(values); setValues({}); return "Сохранено в защищённом хранилище"; })}>Сохранить</button>
        <button className="factory-secondary" disabled={busy || !configured} onClick={() => void run(() => api.integrationTest(kind))}>Проверить</button>
        <button className="factory-secondary" disabled={busy || !configured} onClick={() => void run(async () => { const r = await api.nodeConfigPush([kind]); return r.applied.length ? "Отправлено на сервер" : r.skipped.join("; "); })}>Отправить на сервер</button>
        {serverKnown && <span className="trend-chip">{serverHas ? "на сервере есть" : "на сервере нет"}</span>}
      </div>
    </div>
  </section>;
}

export default function Integrations() {
  const setArea = useUi(s => s.setArea);
  const read = useCallback(() => api.discoveryState(), []);
  const { data, error, refresh } = useFactoryData(read);
  const [remote, setRemote] = useState<RemoteConfig | null>(null);
  const [remoteError, setRemoteError] = useState("");
  const [message, setMessage] = useState("");
  const loadRemote = useCallback(async () => { try { setRemote(await api.nodeConfigRemote()); setRemoteError(""); } catch (e) { setRemote(null); setRemoteError(errText(e)); } }, []);
  useEffect(() => { void loadRemote(); }, [loadRemote]);
  const done = (m: string) => { setMessage(m); void refresh(); void loadRemote(); };
  const has = (k: string) => remote?.secrets[k] ?? false;

  return <div className="factory-page integrations-page">
    <FactoryHeading title="Интеграции" detail="Все внешние ключи и доступы в одном месте. Каждый можно проверить и отправить на сервер." />
    <LoadError error={error} retry={refresh} />
    {message && <p className="analytics-notice" role="status">{message}</p>}
    <p className="analytics-notice">{remote ? `Сервер на связи${remote.keyStore ? "" : ", но хранилище секретов не настроено"}.` : `Сервер недоступен${remoteError ? `: ${remoteError}` : ""}. Сохранение и проверка работают локально, отправка на сервер — после подключения на экране «Сервер».`}</p>
    {data && <div className="trend-cols">
      <KeyCard kind="twitch" title="Twitch" hint="Client ID и Secret приложения с dev.twitch.tv (бесплатно). Нужны для трендов, топа эфиров и клипов зрителей." configured={data.twitchKeys} serverHas={has("factory_twitch_app")} serverKnown={!!remote}
        fields={[{ id: "id", label: "Client ID" }, { id: "secret", label: "Client Secret", secret: true }]} onSave={v => api.twitchSignalCredentials(v.id, v.secret)} onDone={done} />
      <KeyCard kind="youtube" title="YouTube Data API" hint="Ключ из Google Cloud (API YouTube Data v3). Нужен для популярного и поиска эфиров. Вход в аккаунт для публикации задаётся в «Аккаунтах»." configured={data.youtubeKey} serverHas={has("discovery_youtube_key")} serverKnown={!!remote}
        fields={[{ id: "key", label: "API-ключ", secret: true }]} onSave={v => api.discoverySecretSet("youtube", v.key)} onDone={done} />
      <KeyCard kind="kick" title="Kick" hint="Токен приложения Kick (официальный API). Нужен для списка эфиров по зрителям." configured={data.kickToken} serverHas={has("discovery_kick_token")} serverKnown={!!remote}
        fields={[{ id: "token", label: "Токен", secret: true }]} onSave={v => api.discoverySecretSet("kick", v.token)} onDone={done} />
    </div>}
    <NotifyPanel />
    <section className="factory-panel" style={{ marginTop: 16 }}>
      <h2>Настройки поиска на сервере</h2>
      <p>Языки, категории, белый список и пороги с экрана «Тренды» можно отправить на сервер, чтобы ночью он искал по тем же правилам.</p>
      <div className="factory-actions"><button className="factory-secondary" onClick={async () => { try { const r = await api.nodeConfigPush(["discovery"]); done(r.applied.length ? "Настройки поиска отправлены на сервер" : "Нечего отправлять"); } catch (e) { setMessage(errText(e)); } }}>Отправить на сервер</button></div>
    </section>
    <section className="factory-panel" style={{ marginTop: 16 }}>
      <h2>Площадки публикации и сервер</h2>
      <p>TikTok, Telegram-канал, YouTube Shorts, Instagram Reels и VK подключаются отдельно: вход, проверка и тестовая отправка.</p>
      <div className="factory-actions"><button className="factory-secondary" onClick={() => setArea("accounts")}>Открыть «Аккаунты»</button><button className="factory-secondary" onClick={() => setArea("server")}>Открыть «Сервер»</button></div>
    </section>
  </div>;
}
