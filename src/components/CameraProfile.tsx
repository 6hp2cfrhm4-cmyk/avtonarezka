import { useEffect, useRef, useState } from "react";
import { api, errText, inTauri, type CameraFrame, type CameraRect, type CameraScanProgress, type CameraScanResult, type Project } from "../lib/api";
import { Field } from "./FactoryControls";
import { onEvent } from "../lib/events";

const initial: CameraRect = { x: 0.04, y: 0.04, width: 0.25, height: 0.3 };
const clamp = (v: number, min: number, max: number) => Math.min(max, Math.max(min, v));
export default function CameraProfile({ value, source, onChange }: { value: CameraRect | null; source: string; onChange: (v: CameraRect | null) => void }) {
  const [projects, setProjects] = useState<Project[]>([]);
  const [projectId, setProjectId] = useState(0);
  const [at, setAt] = useState(0);
  const [frame, setFrame] = useState<CameraFrame | null>(null);
  const [rect, setRect] = useState<CameraRect>(value ?? initial);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [open, setOpen] = useState(false);
  const [modelReady, setModelReady] = useState(false);
  const [downloading, setDownloading] = useState(false);
  const [faces, setFaces] = useState<Awaited<ReturnType<typeof api.cameraDetect>> | null>(null);
  const [scanProgress, setScanProgress] = useState<CameraScanProgress | null>(null);
  const [scanResult, setScanResult] = useState<CameraScanResult | null>(null);
  const scanId = useRef<string | null>(null);
  useEffect(() => {
    const off = onEvent<CameraScanProgress>("camera-scan-progress", progress => {
      if (progress.id === scanId.current) setScanProgress(progress);
    });
    return () => { off(); if (scanId.current) void api.cameraScanCancel(scanId.current); };
  }, []);
  const drag = useRef<{ x: number; y: number; rect: CameraRect; resize: boolean } | null>(null);
  useEffect(() => {
    if (!open) return;
    let active = true;
    api.listProjects().then(rows => {
      if (!active) return; setProjects(rows); setProjectId(rows.find(p => p.path === source)?.id ?? rows[0]?.id ?? 0);
    }).catch(e => { if (active) setError(errText(e)); });
    return () => { active = false; };
  }, [open, source]);
  useEffect(() => {
    if (!open) return;
    const check = () => void api.faceModelReady().then(ready => { setModelReady(ready); if (ready) setDownloading(false); }).catch(e => setError(errText(e)));
    const off = onEvent("models-done", check);
    const offError = onEvent<{ message: string }>("models-error", event => { setDownloading(false); setError(event.message); });
    check(); return () => { off(); offError(); };
  }, [open]);
  const load = async () => {
    setBusy(true); setError(""); setFaces(null); setScanResult(null);
    try { setFrame(await api.cameraFrame(projectId, at)); } catch (e) { setError(errText(e)); } finally { setBusy(false); }
  };
  const detect = async () => {
    setBusy(true); setError(""); setFaces(null); setScanResult(null);
    try { setFrame(await api.cameraFrame(projectId, at)); setFaces(await api.cameraDetect(projectId, at)); }
    catch (e) { setError(errText(e)); } finally { setBusy(false); }
  };
  const useFace = (face: CameraRect) => {
    setScanResult(null);
    const width = Math.min(1, face.width * 1.8), height = Math.min(1, face.height * 1.8);
    setRect({ x: clamp(face.x + (face.width - width) / 2, 0, 1 - width), y: clamp(face.y + (face.height - height) / 2, 0, 1 - height), width, height });
  };
  const scan = async () => {
    const id = crypto.randomUUID(); scanId.current = id;
    setBusy(true); setError(""); setScanResult(null);
    setScanProgress({ id, frames: 0, total: Math.floor((projects.find(p => p.id === projectId)?.duration ?? 0) * 2), stage: "waiting" });
    try {
      const result = await api.cameraScan(projectId, id, rect);
      setScanResult(result);
      if (result.suggested) setRect(result.suggested);
    } catch (e) { setError(errText(e)); }
    finally { scanId.current = null; setScanProgress(null); setBusy(false); }
  };
  const downloadModel = async () => {
    setError(""); setDownloading(true);
    try { if (!await api.faceModelDownload()) { setModelReady(await api.faceModelReady()); setDownloading(false); } }
    catch (e) { setDownloading(false); setError(errText(e)); }
  };
  const update = (key: keyof CameraRect, percent: number) => {
    setScanResult(null);
    if (!Number.isFinite(percent)) return;
    setRect(r => {
      const n = { ...r, [key]: clamp(percent / 100, key === "x" || key === "y" ? 0 : 0.01, 1) };
      n.x = clamp(n.x, 0, 1 - n.width); n.y = clamp(n.y, 0, 1 - n.height); return n;
    });
  };
  return <section className="camera-profile">
    <div className="factory-section-heading"><h3>Камера стримера</h3><button type="button" className="factory-link" disabled={busy} onClick={() => setOpen(v => !v)}>{open ? "Свернуть камеру" : "Настроить по кадру"}</button></div>
    <p>{value ? `Задана рамка ${Math.round(value.width * 100)} × ${Math.round(value.height * 100)}% кадра. Перед рендером лицо проверяется заново.` : "Выбери область вебкамеры на исходном видео. Профиль сохранится вместе с каналом."}</p>
    {open && <div className="factory-form">
      <div className="factory-fields">
        <Field label="Запись для настройки камеры"><select className="factory-input" disabled={busy} value={projectId} onChange={e => { setProjectId(+e.target.value); setAt(0); setFrame(null); setFaces(null); setScanResult(null); }}><option value={0}>Выбери импортированную запись</option>{projects.map(p => <option key={p.id} value={p.id}>{p.title}</option>)}</select></Field>
        <Field label="Кадр камеры, секунда"><input className="factory-input" disabled={busy} type="number" min={0} max={(projects.find(p => p.id === projectId)?.duration ?? 1) - 0.001} step={0.5} value={at} onChange={e => setAt(+e.target.value)} /></Field>
      </div>
      <div className="factory-actions"><button type="button" className="factory-secondary" disabled={!projectId || busy || !inTauri} onClick={() => void load()}>{busy && !scanProgress ? "Читаю кадр…" : "Показать кадр камеры"}</button>{modelReady ? <button type="button" className="factory-secondary" disabled={!projectId || busy || !inTauri} onClick={() => void detect()}>Найти лица на кадре</button> : <button type="button" className="factory-link" disabled={busy || downloading || !inTauri} onClick={() => void downloadModel()}>{downloading ? "Загрузка YuNet…" : "Скачать YuNet · 230 КБ"}</button>}{value && <button type="button" className="factory-link" disabled={busy} onClick={() => onChange(null)}>Сбросить рамку</button>}</div>
      {faces && <div role="status"><p>{faces.length ? `На кадре найдено лиц: ${faces.length}. Выбери лицо стримера и уточни рамку.` : "Лицо на этом кадре не найдено. Выбери другой момент записи или задай рамку вручную."}</p><div className="factory-actions">{faces.map((f, i) => <button type="button" className="factory-secondary" key={i} disabled={busy} onClick={() => useFace(f.rect)}>Лицо {i + 1} · {Math.round(f.confidence * 100)}%</button>)}</div></div>}
      {!inTauri && <p>Кадры исходника доступны в настольном приложении.</p>}
      {error && <p className="factory-error" role="alert">{error}</p>}
      {scanProgress && <div role="status" aria-live="polite"><p>{scanProgress.stage === "waiting" ? "Ожидаю свободную дорожку ИИ…" : `Проверяю запись: ${scanProgress.frames} из ≈${scanProgress.total} кадров · ${Math.min(100, Math.round(scanProgress.frames / Math.max(1, scanProgress.total) * 100))}%`}</p><progress aria-label="Проверка камеры по записи" max={Math.max(1, scanProgress.total)} value={scanProgress.frames} /><button type="button" className="factory-secondary" onClick={() => { if (scanId.current) void api.cameraScanCancel(scanId.current); }}>Остановить проверку</button></div>}
      {scanResult && <p role="status">{scanResult.suggested ? `Предложена медианная рамка. Лицо найдено на ${scanResult.check.found} из ${scanResult.check.frames} кадров; самый длинный пропуск — ${(scanResult.check.longestGap / 2).toFixed(1)} с. Уточни рамку на кадре и прими её.` : `Устойчивая рамка не найдена: лицо на ${scanResult.check.found} из ${scanResult.check.frames} кадров. Уточни область камеры или выбери другой кадр.`}</p>}
      {frame && <>
        <div className="factory-actions"><button type="button" className="factory-secondary" disabled={busy || !modelReady} onClick={() => void scan()}>Проверить всю запись</button><span>2 кадра/с в выбранной области. Рамка будет предложена для подтверждения.</span></div>
        <div className="camera-frame" style={{ maxWidth: Math.min(640, 300 * frame.width / frame.height) }} data-camera-project={projectId} onPointerMove={e => {
          const d = drag.current; if (!d || busy) return; setScanResult(null); const bounds = e.currentTarget.getBoundingClientRect();
          const dx = (e.clientX - d.x) / bounds.width, dy = (e.clientY - d.y) / bounds.height;
          setRect(d.resize ? { ...d.rect, width: clamp(d.rect.width + dx, 0.01, 1 - d.rect.x), height: clamp(d.rect.height + dy, 0.01, 1 - d.rect.y) } : { ...d.rect, x: clamp(d.rect.x + dx, 0, 1 - d.rect.width), y: clamp(d.rect.y + dy, 0, 1 - d.rect.height) });
        }} onPointerUp={() => { drag.current = null; }} onPointerCancel={() => { drag.current = null; }}>
          <img src={frame.url} alt={`Исходная запись, секунда ${frame.at}`} draggable={false} />
          <div className="camera-rect" style={{ left: `${rect.x * 100}%`, top: `${rect.y * 100}%`, width: `${rect.width * 100}%`, height: `${rect.height * 100}%` }} onPointerDown={e => {
            e.preventDefault(); if (busy) return; e.currentTarget.parentElement?.setPointerCapture(e.pointerId);
            drag.current = { x: e.clientX, y: e.clientY, rect, resize: (e.target as HTMLElement).dataset.resize === "true" };
          }}><span>Область камеры</span><i data-resize="true" title="Изменить размер рамки" /></div>
        </div>
        <p>Перетащи рамку за середину, измени размер за нижний правый угол или задай проценты.</p>
        <div className="factory-fields">{([["x", "Камера: слева, %"], ["y", "Камера: сверху, %"], ["width", "Камера: ширина, %"], ["height", "Камера: высота, %"]] as const).map(([key, label]) => <Field key={key} label={label}><input className="factory-input" disabled={busy} type="number" step={0.1} min={key === "x" || key === "y" ? 0 : 1} max={100} value={Math.round(rect[key] * 1000) / 10} onChange={e => update(key, +e.target.value)} /></Field>)}</div>
        <div className="factory-actions"><button type="button" className="factory-secondary" disabled={busy} onClick={() => onChange(rect)}>Принять рамку камеры</button><span>{value && Object.keys(rect).every(k => rect[k as keyof CameraRect] === value[k as keyof CameraRect]) ? "Рамка принята. Сохрани профиль канала." : "Рамка ещё не принята"}</span></div>
      </>}
    </div>}
  </section>;
}
