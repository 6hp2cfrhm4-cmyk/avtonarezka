import { useCallback, useState } from "react";
import { api, errText, inTauri, type Channel, type ChannelIncident } from "../lib/api";
import { Field, LoadError, useFactoryData } from "./FactoryControls";
import VirtualRows from "./VirtualRows";

export default function ChannelRights({ channel, onChanged }: { channel: Channel; onChanged: () => Promise<void> }) {
  const read = useCallback(async () => { const [incidents, channels] = await Promise.all([api.channelIncidents(channel.id), api.channelsList()]); return { incidents, channel: channels.find(c => c.id === channel.id) }; }, [channel.id]);
  const { data, error, refresh } = useFactoryData(read);
  const current = data?.channel ?? channel;
  const [kind, setKind] = useState<ChannelIncident["kind"]>("complaint");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const act = async (fn: () => Promise<unknown>, success: string) => {
    setBusy(true); setMessage("");
    try { await fn(); await refresh(); await onChanged(); setMessage(success); }
    catch (e) { setMessage(errText(e)); } finally { setBusy(false); }
  };
  return <section className="channel-rights factory-form" aria-label="Жалобы и ограничения">
    <div className="factory-section-heading"><h3>Жалобы и ограничения</h3><span>Неразобрано: {current.activeIncidents}</span></div>
    {current.rightsPaused && <p role="status">Канал на паузе. Опрос, следующие стадии обработки и ожидающие публикации остановлены. Текущая операция завершается.</p>}
    <p>События добавляются вручную по уведомлениям площадок. Порог берётся из сохранённого профиля: {current.complaintLimit}.</p>
    <div className="factory-fields">
      <Field label="Вид события"><select className="factory-input" value={kind} onChange={e => setKind(e.target.value as ChannelIncident["kind"])}><option value="complaint">Жалоба</option><option value="restriction">Ограничение площадки</option></select></Field>
      <Field label="Что произошло"><input className="factory-input" value={note} maxLength={1000} onChange={e => setNote(e.target.value)} placeholder="Причина из уведомления площадки" /></Field>
    </div>
    <div className="factory-actions"><button type="button" className="factory-secondary" disabled={busy || !inTauri || !note.trim()} onClick={() => void act(async () => { await api.channelIncidentAdd({ channelId: channel.id, publicationId: null, kind, note }); setNote(""); }, "Событие записано")}>Записать событие</button>
      {current.rightsPaused && <button type="button" className="factory-secondary" disabled={busy || !inTauri || current.activeIncidents >= current.complaintLimit} onClick={() => void act(() => api.channelRightsResume(channel.id), "Пауза канала снята. Ожидающие публикации доступны в календаре")}>Возобновить канал</button>}
    </div>
    <LoadError error={error} retry={() => void refresh()} />
    {data && data.incidents.length === 0 && <p className="factory-empty">Жалоб и ограничений пока нет.</p>}
    <VirtualRows items={data?.incidents ?? []} label="Последние 100 событий канала" rowHeight={120} render={incident => <div className="factory-object-row">
      <span><strong>{incident.kind === "complaint" ? "Жалоба" : "Ограничение"} · {incident.resolvedAt ? "Разобрано" : "Требует разбора"}</strong><small>{new Date(incident.createdAt * 1000).toLocaleString("ru-RU")}{incident.publicationId ? ` · Публикация #${incident.publicationId}` : ""}</small><small title={incident.note}>{incident.note}</small></span>
      {!incident.resolvedAt && <button type="button" className="factory-link" disabled={busy || !inTauri} onClick={() => void act(() => api.channelIncidentResolve(incident.id), "Событие разобрано; пауза снимается отдельно")}>Разобрано</button>}
    </div>} />
    {message && <p role="status">{message}</p>}
  </section>;
}
