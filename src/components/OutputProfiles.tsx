import { useEffect, useState } from "react";
import { api, errText, inTauri, type ChannelTarget, type OutputProfile, type OutputVariant } from "../lib/api";
import { Field, LoadError } from "./FactoryControls";
import { onEvent } from "../lib/events";
const names: Record<ChannelTarget["platform"], string> = { tiktok: "TikTok", telegram: "Telegram", youtube: "YouTube Shorts", instagram: "Instagram Reels", vk: "VK Клипы" };
export function OutputProfiles() {
  const [profiles, setProfiles] = useState<OutputProfile[]>([]);
  const [platform, setPlatform] = useState<ChannelTarget["platform"]>("telegram");
  const [editing, setEditing] = useState<OutputProfile | null>(null);
  const [error, setError] = useState(""); const [busy, setBusy] = useState(false); const [message, setMessage] = useState("");
  const read = async () => { try { setProfiles(await api.outputProfiles()); setError(""); } catch (e) { setError(errText(e)); } };
  useEffect(() => { void read(); }, []);
  useEffect(() => { setEditing(profiles.find(p => p.platform === platform) ?? null); }, [platform, profiles]);
  useEffect(() => { setMessage(""); }, [platform]);
  const patch = (p: Partial<OutputProfile["limits"]>) => { setEditing(v => v && ({ ...v, limits: { ...v.limits, ...p } })); setMessage(""); };
  return <section className="factory-panel factory-form output-profiles">
    <h2>Выходные варианты</h2>
    <p>Отдельный файл для площадки: 1080 × 1920, H.264/AAC. Предел длительности обрезает конец варианта. Исходный финал сохраняется. В импортированных готовых файлах надпись «Часть N» уже запечена и остаётся.</p>
    <LoadError error={error} retry={() => void read()} />
    <Field label="Площадка выходного профиля"><select className="factory-input" value={platform} disabled={busy} onChange={e => setPlatform(e.target.value as ChannelTarget["platform"])}>{profiles.map(p => <option key={p.platform} value={p.platform}>{names[p.platform]}</option>)}</select></Field>
    {editing && <form className="factory-form" onSubmit={async e => { e.preventDefault(); setBusy(true); setError(""); try { const saved = await api.outputProfileSave(editing); setProfiles(all => all.map(p => p.platform === saved.platform ? saved : p)); setMessage("Профиль сохранён. Он действует на новые слоты и предпросмотры"); } catch (e) { setError(errText(e)); } finally { setBusy(false); } }}>
      <div className="factory-fields output-profile-limits">
        <Field label="Предел длительности, с" hint="Пусто: целиком, если площадка не ограничивает длину."><input className="factory-input" type="number" min={editing.platform === "instagram" ? 3 : 1} max={editing.platform === "youtube" ? 180 : editing.platform === "instagram" ? 900 : 3600} step={1} required={["youtube", "instagram"].includes(editing.platform)} disabled={busy} value={editing.limits.maxSeconds ?? ""} onChange={e => patch({ maxSeconds: e.target.value ? +e.target.value : null })} /></Field>
        <Field label="Битрейт видео, кбит/с"><input className="factory-input" type="number" min={200} max={30000} step={100} required disabled={busy} value={editing.limits.videoKbps} onChange={e => patch({ videoKbps: +e.target.value })} /></Field>
        <Field label="Битрейт аудио, кбит/с"><select className="factory-input" disabled={busy} value={editing.limits.audioKbps} onChange={e => patch({ audioKbps: +e.target.value })}>{[64, 96, 128, 160, 192, 256, 320].map(n => <option key={n}>{n}</option>)}</select></Field>
      </div>
      <p>Для Telegram битрейт дополнительно снижается под лимит 50 МБ. При нескольких включённых аккаунтах площадки или нескольких аккаунтах в календаре этого ролика надпись «Часть N» убирается.</p>
      <div className="factory-actions"><button className="factory-secondary" disabled={busy || !inTauri}>{busy ? "Сохраняю…" : "Сохранить выходной профиль"}</button><span role="status">{message}</span></div>
    </form>}
  </section>;
}
export function OutputPreview({ clips }: { clips: { id: number; title: string }[] }) {
  const [id, setId] = useState(clips[0]?.id ?? 0); const [platform, setPlatform] = useState<ChannelTarget["platform"]>("telegram");
  const [profiles, setProfiles] = useState<OutputProfile[]>([]); const [result, setResult] = useState<OutputVariant | null>(null);
  const [fraction, setFraction] = useState<number | null>(null);
  const [busy, setBusy] = useState(false); const [error, setError] = useState("");
  useEffect(() => { api.outputProfiles().then(setProfiles).catch(e => setError(errText(e))); }, []);
  const selected = clips.some(c => c.id === id) ? id : clips[0]?.id ?? 0;
  useEffect(() => onEvent<{ draftId: number; platform: ChannelTarget["platform"]; fraction: number }>("output-progress", p => {
    if (busy && p.draftId === selected && p.platform === platform) setFraction(Math.max(0, Math.min(1, p.fraction)));
  }), [busy, selected, platform]);
  useEffect(() => { setResult(null); setError(""); }, [selected, platform]);
  if (!clips.length) return null;
  return <section className="factory-panel factory-form output-preview">
    <h2>Вариант для площадки</h2>
    <p>Проверь результат перед назначением слота. В календаре применяются настройки, сохранённые на момент назначения.</p>
    <div className={result ? "output-preview-content has-video" : "output-preview-content"}><div className="factory-form">
    <div className="factory-fields"><Field label="Ролик выходного варианта"><select className="factory-input" value={selected} disabled={busy} onChange={e => setId(+e.target.value)}>{clips.map(c => <option key={c.id} value={c.id}>{c.title || `Ролик #${c.id}`}</option>)}</select></Field><Field label="Площадка выходного варианта"><select className="factory-input" value={platform} disabled={busy} onChange={e => setPlatform(e.target.value as ChannelTarget["platform"])}>{profiles.map(p => <option key={p.platform} value={p.platform}>{names[p.platform]}</option>)}</select></Field></div>
    <button type="button" className="factory-secondary" disabled={busy || !inTauri || !selected} onClick={async () => { setBusy(true); setFraction(null); setResult(null); setError(""); try { setResult(await api.outputPreview(selected, platform)); } catch (e) { setError(errText(e)); } finally { setBusy(false); } }}>{busy ? "Готовлю выходной вариант…" : "Подготовить и проверить вариант"}</button>
    {busy && <p role="status">{fraction === null ? "Подготавливаю исходники и проверяю сохранённую рамку лица…" : `Рендер выходного варианта: ${Math.round(fraction * 100)}%`}</p>}
    {error && <p role="alert" className="factory-error">{error}</p>}
    {result && <div className="factory-form"><p role="status">{result.duration.toFixed(2)} с · {(result.bytes / 1_000_000).toFixed(2)} МБ · {result.videoKbps} кбит/с{result.partRemoved ? " · без «Части N»" : ""}{result.truncated ? " · конец обрезан по профилю" : ""}</p><a className="factory-link" href={result.url} target="_blank" rel="noreferrer">Открыть выходной файл</a></div>}
    </div>{result && <video className="output-preview-video" aria-label="Предпросмотр выходного варианта" controls preload="metadata" poster={result.posterUrl} src={result.url} />}</div>
  </section>;
}
