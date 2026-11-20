import { useState } from "react";
import { api, defaultStyle, errText, inTauri, type Defaults, type FactoryTemplate, type Channel, type Project } from "../lib/api";
import { useUi } from "../store";
import { FactoryHeading, Field, LoadError, useFactoryData } from "../components/FactoryControls";
import { Color, Row, Slide } from "../components/StyleControls";
import VirtualRows from "../components/VirtualRows";
const read = async () => ({ templates: await api.templatesList(), assets: await api.assetsList(), channels: await api.channelsList(), projects: await api.listProjects() });
const layouts = { gameplay_stack: "Видео + геймплей", reaction_pip: "Реакция · круг с лицом", reaction_stack: "Реакция · отдельная область" };

function TemplatePreview({ template, channels, projects, onReady }: { template: FactoryTemplate; channels: Channel[]; projects: Project[]; onReady: () => void }) {
  const [channel, setChannel] = useState(channels.find(c => c.templateId === template.id)?.id ?? channels[0]?.id ?? 0);
  const [project, setProject] = useState(projects.find(p => p.path === channels.find(c => c.id === channel)?.source)?.id ?? projects[0]?.id ?? 0);
  const [at, setAt] = useState(0);
  const [frame, setFrame] = useState<{ url: string; note: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const preview = async () => {
    setBusy(true); setError("");
    try { setFrame(await api.layoutPreview(channel, project, at, template)); onReady(); }
    catch (e) { setError(errText(e)); } finally { setBusy(false); }
  };
  return <section className="template-preview-section">
    <h3>Кадр из твоей записи</h3>
    <div className="template-preview-workspace"><div className="factory-form">
      <Field label="Канал для предпросмотра"><select className="factory-input" value={channel} onChange={e => { const id = +e.target.value; setChannel(id); const p = projects.find(p => p.path === channels.find(c => c.id === id)?.source); if (p) setProject(p.id); }}><option value={0}>Выбери канал</option>{channels.map(c => <option value={c.id} key={c.id}>{c.name}</option>)}</select></Field>
      <Field label="Запись для предпросмотра"><select className="factory-input" value={project} onChange={e => { setProject(+e.target.value); setAt(0); }}><option value={0}>Выбери запись</option>{projects.map(p => <option value={p.id} key={p.id}>{p.title}</option>)}</select></Field>
      <Field label="Секунда предпросмотра"><input className="factory-input" type="number" min={0} max={(projects.find(p => p.id === project)?.duration ?? 1) - 0.5} step={0.5} value={at} onChange={e => setAt(+e.target.value)} /></Field>
      <button type="button" className="factory-secondary" disabled={!inTauri || !project || !channel || busy} onClick={() => void preview()}>{busy ? "Собираю кадр…" : "Обновить предпросмотр"}</button>
      <p>Текущие настройки, настоящий кадр и ассеты. Для финального ролика лицо проверяется по всему выбранному фрагменту.</p>
      {!inTauri && <p>Предпросмотр доступен в настольном приложении.</p>}
      {error && <p className="factory-error" role="alert">{error}</p>}
      {frame?.note && <p role="status">{frame.note}</p>}
    </div><div className="template-preview-image">{frame?.url || template.previewUrl ? <img src={frame?.url ?? template.previewUrl} alt="Предпросмотр шаблона на исходной записи" /> : <p>Выбери запись и собери кадр</p>}</div></div>
  </section>;
}
export default function Templates() {
  const { data, error, refresh } = useFactoryData(read);
  const [editing, setEditing] = useState<FactoryTemplate | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const notify = useUi(s => s.notify);
  const patch = (fn: (d: Defaults) => void) => setEditing(t => { if (!t) return null; const next = structuredClone(t); fn(next.config); return next; });
  const save = async () => { if (!editing) return; setBusy(true); setMessage(""); try { const saved = await api.templateSave(editing); setEditing(saved); await refresh(); useUi.getState().setDefaults(await api.defaultsGet()); notify("Шаблон сохранён для новых источников"); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } };
  return <div className="factory-page">
    <FactoryHeading title="Шаблоны" detail="Оформление и ассеты для новых черновиков канала."><button className="factory-primary" disabled={!data?.templates.length} onClick={() => { const base = data?.templates[0]; if (base) { setEditing({ ...structuredClone(base), id: 0, name: "Новый шаблон", isDefault: false }); setMessage(""); } }}>Новый шаблон</button></FactoryHeading>
    <LoadError error={error} retry={refresh} />
    {!data && !error && <p>Загружаю шаблоны…</p>}
    <div className={editing ? "factory-editor-grid" : ""}>
      <section className="factory-panel"><h2>Оформление · {data?.templates.length ?? 0}</h2><VirtualRows items={data?.templates ?? []} label="Шаблоны оформления" rowHeight={112} render={t => <button data-template-id={t.id} className={`factory-object-row template-gallery-row ${editing?.id === t.id ? "is-selected" : ""}`} onClick={() => { setEditing(structuredClone(t)); setMessage(""); }}>{t.previewUrl && <img src={t.previewUrl} alt={`Кадр: ${t.name}`} />}<span><strong>{t.name}</strong><small>{layouts[t.layout]} · {t.config.style.subtitles.enabled ? "С субтитрами" : "Без субтитров"}</small></span>{t.isDefault && <span>Студия</span>}</button>} /></section>
      {editing && <section className="factory-panel factory-form">
        <div className="factory-section-heading"><h2>{editing.id ? "Редактор шаблона" : "Новый шаблон"}</h2><button className="factory-link" onClick={() => setEditing(null)}>Закрыть</button></div>
        <Field label="Название"><input className="factory-input" value={editing.name} onChange={e => setEditing({ ...editing, name: e.target.value })} /></Field>
        {editing.isDefault && <p>Этот шаблон также используется в ручной Студии. Уже созданные черновики сохраняют своё оформление.</p>}
        <Field label="Раскладка"><select className="factory-input" value={editing.layout} onChange={e => {
          const layout = e.target.value as FactoryTemplate["layout"];
          setEditing(t => { if (!t) return null; const n = structuredClone(t); n.layout = layout; n.config.style.layout = ({ gameplay_stack: "gameplayStack", reaction_pip: "reactionPip", reaction_stack: "reactionStack" } as const)[layout];
            n.config.style.reaction ??= structuredClone(defaultStyle.reaction);
            if (layout === "reaction_pip") { n.config.style.split = 0.34; n.config.style.subtitles.yPct = 0.52; }
            else if (layout === "reaction_stack") n.config.style.subtitles.yPct = 0.48;
            return n; });
        }}>{Object.entries(layouts).map(([id, name]) => <option key={id} value={id}>{name}</option>)}</select></Field>
        <TemplatePreview key={editing.id} template={editing} channels={data?.channels ?? []} projects={data?.projects ?? []} onReady={() => void refresh()} />
        {editing.layout !== "gameplay_stack" && <details open className="factory-details"><summary>Лицо и фон</summary><div className="factory-form">
          {editing.layout === "reaction_pip" && <>
            <Row label="Геймплей под основным видео · вариант A" value={editing.config.style.reaction.background} onChange={v => patch(d => { d.style.reaction.background = v; if (!v) d.style.subtitles.yPct = 0.72; })} />
            <Row label="Круглая маска" value={editing.config.style.reaction.circle} onChange={v => patch(d => { d.style.reaction.circle = v; })} />
            <Field label="Положение лица"><select className="factory-input" value={editing.config.style.reaction.position} onChange={e => patch(d => { d.style.reaction.position = e.target.value as Defaults["style"]["reaction"]["position"]; })}><option value="seamLeft">На стыке слева</option><option value="seamCenter">На стыке по центру</option><option value="bottomRight">Справа внизу</option></select></Field>
            <Slide label="Размер лица" value={editing.config.style.reaction.sizePct} min={0.15} max={0.6} fmt={v => `${Math.round(v * 100)}% ширины`} onChange={v => patch(d => { d.style.reaction.sizePct = v; })} />
            <Slide label="Толщина обводки" value={editing.config.style.reaction.ringPx} min={0} max={20} step={1} onChange={v => patch(d => { d.style.reaction.ringPx = v; })} />
            <Color label="Цвет обводки" value={editing.config.style.reaction.ringColor} onChange={v => patch(d => { d.style.reaction.ringColor = v; })} />
            {/* taste-skill-ignore: shadow — эффект экспортируемого видео, а не тень интерфейса */} <Row label="Тень от круга" value={editing.config.style.reaction.shadow} onChange={v => patch(d => { d.style.reaction.shadow = v; })} />
            <Row label="Появление с увеличением" value={editing.config.style.reaction.pop} onChange={v => patch(d => { d.style.reaction.pop = v; })} />
          </>}
          <Slide label="Приближение лица" value={editing.config.style.reaction.zoom} min={1} max={2} onChange={v => patch(d => { d.style.reaction.zoom = v; })} />
          <Row label="Лицо замирает во время баннера" value={editing.config.style.reaction.freeze} onChange={v => patch(d => { d.style.reaction.freeze = v; })} />
          <p>Область камеры задаётся в профиле канала. Если лицо исчезнет, ролик попадёт в Inbox с причиной.</p>
        </div></details>}
        <Slide label="Доля верхнего видео" value={editing.config.style.split} min={0.3} max={0.75} fmt={v => `${Math.round(v * 100)}%`} onChange={v => patch(d => { d.style.split = v; })} />
        <Field label="Вписывание видео"><select className="factory-input" value={editing.config.style.topFit} onChange={e => patch(d => { d.style.topFit = e.target.value as Defaults["style"]["topFit"]; })}><option value="auto">Авто: 16:9 целиком</option><option value="contain">Вписать с размытием</option><option value="fill">Заполнить кадр</option></select></Field>
        <Slide label="Срезать снизу" value={editing.config.style.topCropBottom} min={0} max={0.4} fmt={v => `${Math.round(v * 100)}%`} onChange={v => patch(d => { d.style.topCropBottom = v; })} />
        <Row label="Геймплей замирает на баннере" value={editing.config.style.gameplayFreeze} onChange={v => patch(d => { d.style.gameplayFreeze = v; })} />
        <details open className="factory-details"><summary>Ассеты</summary><div className="factory-form">
          <Field label="Баннер"><select className="factory-input" value={editing.config.bannerId ?? ""} onChange={e => patch(d => { d.bannerId = +e.target.value || null; })}><option value="">Без баннера</option>{data?.assets.filter(a => a.kind === "banner").map(a => <option key={a.id} value={a.id}>{a.name}</option>)}</select></Field>
          <Row label="Показывать баннер" value={editing.config.style.banner.enabled} onChange={v => patch(d => { d.style.banner.enabled = v; })} />
          <Field label="Музыка"><select className="factory-input" value={editing.config.musicId ?? ""} onChange={e => patch(d => { d.musicId = +e.target.value || null; })}><option value="">Без музыки</option>{data?.assets.filter(a => a.kind === "music").map(a => <option key={a.id} value={a.id}>{a.name}</option>)}</select></Field>
          <div><div className="mb-2 text-sm">Фоны по порядку</div><div className="factory-checks">{data?.assets.filter(a => a.kind === "background").map(a => <label key={a.id}><input type="checkbox" checked={editing.config.backgroundIds.includes(a.id)} onChange={e => patch(d => { d.backgroundIds = e.target.checked ? [...d.backgroundIds, a.id] : d.backgroundIds.filter(id => id !== a.id); })} />{a.name}</label>)}</div>{!data?.assets.some(a => a.kind === "background") && <p>Библиотека фонов пуста. Добавь геймплей в Студии → Импорт.</p>}</div>
        </div></details>
        <Row label="Заголовок LLM в первые 3 секунды" value={editing.config.style.hookEnabled ?? false} onChange={v => patch(d => { d.style.hookEnabled = v; })} />
        <details open className="factory-details"><summary>Субтитры</summary><div className="factory-form">
          <Row label="Субтитры по словам" value={editing.config.style.subtitles.enabled} onChange={v => patch(d => { d.style.subtitles.enabled = v; })} />
          <Slide label="Размер шрифта" value={editing.config.style.subtitles.size} min={24} max={100} step={1} onChange={v => patch(d => { d.style.subtitles.size = v; })} />
          <Slide label="Положение по высоте" value={editing.config.style.subtitles.yPct ?? editing.config.style.split} min={0.1} max={0.9} onChange={v => patch(d => { d.style.subtitles.yPct = v; })} />
          <Color label="Цвет текста" value={editing.config.style.subtitles.color} onChange={v => patch(d => { d.style.subtitles.color = v; })} />
          <Color label="Выделенное слово" value={editing.config.style.subtitles.highlight} onChange={v => patch(d => { d.style.subtitles.highlight = v; })} />
        </div></details>
        <div className="factory-fields">
          <Slide label="Громкость исходника" value={editing.config.style.audio.main} min={0} max={2} onChange={v => patch(d => { d.style.audio.main = v; })} />
          <Slide label="Громкость музыки" value={editing.config.style.audio.music} min={0} max={1} onChange={v => patch(d => { d.style.audio.music = v; })} />
        </div>
        {message && <div className="factory-error" role="alert">{message}</div>}
        <div className="factory-actions"><button className="factory-primary" disabled={busy} onClick={() => void save()}>{busy ? "Сохраняю…" : "Сохранить шаблон"}</button><button className="factory-link" disabled={busy} onClick={() => { setEditing({ ...structuredClone(editing), id: 0, name: `${editing.name} — копия`, isDefault: false }); }}>Сделать копию</button>{editing.id > 0 && !editing.isDefault && <button className="factory-link" disabled={busy} onClick={async () => { try { await api.templateRemove(editing.id); setEditing(null); await refresh(); } catch (e) { setMessage(errText(e)); } }}>Удалить</button>}</div>
      </section>}
    </div>
  </div>;
}
