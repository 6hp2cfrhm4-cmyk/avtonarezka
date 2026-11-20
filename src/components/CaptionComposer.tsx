import { useEffect, useRef, useState } from "react";
import { api, errText, inTauri, type ChannelTarget, type ComposedCaption, type PlatformCaps, type SavedCaption, type PublicationAccount } from "../lib/api";
import { Field } from "./FactoryControls";
import { onEvent } from "../lib/events";
export default function CaptionComposer({ clips }: { clips: { id: number; title: string }[] }) {
  const [id, setId] = useState(clips[0]?.id ?? 0);
  const [platform, setPlatform] = useState<ChannelTarget["platform"]>("telegram");
  const [caps, setCaps] = useState<PlatformCaps[]>([]);
  const [saved, setSaved] = useState<SavedCaption | null>(null);
  const [result, setResult] = useState<ComposedCaption | null>(null);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState("");
  const [accounts, setAccounts] = useState<PublicationAccount[]>([]);
  const [accountId, setAccountId] = useState<number | null>(null);
  const request = useRef(0);
  const selectedId = clips.some(c => c.id === id) ? id : clips[0]?.id ?? 0;
  useEffect(() => onEvent<{ draftId: number; platform: string; accountId: number | null; message: string }>("caption-progress", event => {
    if (busy && event.draftId === selectedId && event.platform === platform && event.accountId === accountId) setProgress(event.message);
  }), [busy, selectedId, platform, accountId]);
  useEffect(() => { api.platformCaps().then(setCaps).catch(e => setError(errText(e))); api.publicationAccounts().then(setAccounts).catch(e => setError(errText(e))); }, []);
  useEffect(() => {
    let active = true; setSaved(null); setResult(null); setError(""); setMessage("");
    request.current++; setBusy(false); setProgress("");
    if (selectedId) api.captionAccountLoad(selectedId, platform, accountId).then(v => { if (active) setSaved(v); }).catch(e => { if (active) setError(errText(e)); });
    return () => { active = false; request.current++; };
  }, [selectedId, platform, accountId]);
  const patch = (fields: Partial<SavedCaption["fields"]>) => { setSaved(s => s && ({ ...s, fields: { ...s.fields, ...fields } })); setResult(null); setMessage(""); };
  const run = async (action: "save" | "tags" | "all") => {
    if (!saved) return;
    const token = ++request.current; setBusy(true); setError(""); setMessage(""); setResult(null);
    setProgress(action === "tags" ? "Жду освобождения локальной модели…" : "Сохраняю текст…");
    try {
      if (action === "tags") {
        const out = await api.captionDraftGenerate(selectedId, saved, accountId);
        if (request.current === token) { setSaved(out.saved); setResult(out.composed); }
      } else if (action === "all") {
        const count = await api.captionApplyBody(clips.map(c => c.id), platform, accountId, saved.fields.body);
        const current = await api.captionAccountLoad(selectedId, platform, accountId);
        if (request.current === token) { setSaved(current); setMessage(`Общий текст сохранён для роликов: ${count}.`); }
      } else {
        const out = await api.captionAccountSave(selectedId, saved, accountId);
        if (request.current === token) setResult(out);
      }
    } catch (e) { if (request.current === token) setError(errText(e)); }
    finally { if (request.current === token) { setBusy(false); setProgress(""); } }
  };
  if (!clips.length) return null;
  return <section className="factory-panel factory-form caption-composer">
    <div className="factory-section-heading"><h2>Подпись для площадки</h2><span>До {caps.find(c => c.platform === platform)?.captionMax ?? "…"} знаков</span></div>
    {busy && progress && <p className="factory-notice" role="status" aria-live="polite">{progress}</p>}
    <p className="factory-muted">Текст необязателен. Вставь свой для выбранного ролика или примени его ко всем готовым роликам этого экрана.</p>
    <div className="factory-fields">
      <Field label="Ролик для подписи"><select className="factory-input" disabled={busy} value={selectedId} onChange={e => setId(+e.target.value)}>{clips.map(c => <option key={c.id} value={c.id}>{c.title || `Ролик #${c.id}`}</option>)}</select></Field>
      <Field label="Площадка подписи"><select className="factory-input" disabled={busy} value={platform} onChange={e => { setPlatform(e.target.value as ChannelTarget["platform"]); setAccountId(null); }}>{caps.map(c => <option key={c.platform} value={c.platform}>{c.name}</option>)}</select></Field>
      <Field label="Аккаунт подписи"><select className="factory-input" disabled={busy} value={accountId ?? ""} onChange={e => setAccountId(e.target.value ? +e.target.value : null)}><option value="">Общая подпись площадки</option>{accounts.filter(a => a.platform === platform).map(a => <option key={a.id} value={a.id}>{a.label}</option>)}</select></Field>
    </div>
    {saved && <fieldset className="factory-form border-0 p-0 m-0 min-w-0" disabled={busy}>
      <Field label={platform === "youtube" ? "Заголовок YouTube" : "Название в подписи"} hint={platform === "youtube" ? "Обязательное название видео, до 100 знаков. Сам текст описания можно оставить пустым." : "Необязательно. Добавляется, если в шаблоне есть поле {название}."}><input className="factory-input" value={saved.fields.title} onChange={e => patch({ title: e.target.value })} /></Field>
      <Field label="Текст ролика" hint="Можно оставить пустым."><textarea className="factory-input" rows={4} value={saved.fields.body} onChange={e => patch({ body: e.target.value })} placeholder="Вставь свой текст или оставь без текста" /></Field>
      <div className="factory-actions"><button className="factory-primary" type="button" disabled={!inTauri} onClick={() => void run("save")}>Проверить и сохранить подпись</button><button className="factory-secondary" type="button" disabled={!inTauri || clips.length < 2} onClick={() => void run("all")}>Применить текст ко всем ({clips.length})</button></div>
      <p className="factory-muted">Общий текст применяется к выбранной площадке и аккаунту. При выборе общей подписи он заменяет текст и в сохранённых вариантах аккаунтов. Хэштеги каждого ролика сохраняются. Подпись используется при назначении нового слота.</p>
      {saved.fields.credit && <p className="factory-notice">Кредит из профиля: {saved.fields.credit}</p>}
      {saved.adLocked && <p className="factory-notice">Ссылка и промокод закреплены кампанией этого ролика.</p>}
      <div className="factory-fields"><Field label="Рекламная ссылка"><input className="factory-input" type="url" readOnly={saved.adLocked} value={saved.fields.link} onChange={e => patch({ link: e.target.value })} placeholder="https://…" /></Field><Field label="Промокод"><input className="factory-input" readOnly={saved.adLocked} value={saved.fields.promo} onChange={e => patch({ promo: e.target.value })} /></Field></div>
      <Field label="Хэштеги подписи"><input className="factory-input" value={saved.fields.hashtags.join(" ")} onChange={e => patch({ hashtags: e.target.value.split(/\s+/).filter(Boolean) })} /></Field>
      <div className="factory-actions"><button className="factory-secondary" type="button" disabled={!inTauri} onClick={() => void run("tags")}>Сгенерировать хэштеги</button></div>
      <p className="factory-muted">Локальная модель подбирает только хэштеги. Текст ты задаёшь сам.</p>
      <details className="factory-details"><summary>Шаблон подписи</summary><Field label="Поля шаблона" hint="{название} {тело} {кредит} {ссылка} {промокод} {хэштеги}. Кредит и реклама добавляются из профиля."><textarea className="factory-input" rows={4} value={saved.spec.template} onChange={e => { setSaved({ ...saved, spec: { ...saved.spec, template: e.target.value } }); setResult(null); }} /></Field></details>
      {!inTauri && <p>Сохранение и локальная генерация доступны в настольном приложении.</p>}
    </fieldset>}
    {message && <p className="factory-notice" role="status">{message}</p>}
    {error && <p className="factory-error" role="alert">{error}</p>}
    {result && <div className="caption-preview" role="status"><strong>Сохранено · {result.len} / {result.max} знаков</strong>{result.bodyTrimmed && <p>Текст сокращён под лимит. Кредит, ссылка и промокод сохранены целиком.</p>}<p>{result.text || "Без подписи"}</p></div>}
  </section>;
}
