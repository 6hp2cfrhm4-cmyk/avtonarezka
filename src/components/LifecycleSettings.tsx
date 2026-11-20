import { useState } from "react";
import { api, errText, type LifecycleStatus } from "../lib/api";
import { LoadError, useFactoryData } from "./FactoryControls";
const read = api.lifecycleStatus;
export default function LifecycleSettings() {
  const { data, error, refresh } = useFactoryData(read);
  const [busy, setBusy] = useState(false); const [message, setMessage] = useState("");
  const change = async (action: () => Promise<LifecycleStatus>) => { setBusy(true); setMessage(""); try { await action(); await refresh(); setMessage("Настройка сохранена"); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } };
  return <section className="factory-panel" aria-label="Трей и автозапуск"><h2>Трей и запуск при входе</h2><LoadError error={error} retry={refresh} />
    <label className="factory-check"><input type="checkbox" checked={data?.trayEnabled ?? false} disabled={!data || busy} onChange={e => void change(() => api.lifecycleTraySet(e.target.checked))} />Показывать значок в трее</label>
    <p>Скрытое окно продолжает обработку. Через меню значка можно вернуть окно или завершить приложение. Закрытие окна крестиком завершает приложение.</p>
    {data?.trayError && <p className="factory-error">{data.trayError}</p>}
    <div className="factory-actions"><button className="factory-secondary" disabled={busy || !data?.trayActive} onClick={async () => { setMessage(""); try { await api.lifecycleHide(); } catch (e) { setMessage(errText(e)); } }}>Скрыть окно в трей</button><span>Перед скрытием проверь, что значок появился в панели рабочего стола.</span></div>
    <label className="factory-check mt-4"><input type="checkbox" checked={data?.autostartEnabled ?? false} disabled={!data?.autostartAvailable || !!data.autostartError || busy} onChange={e => void change(() => api.lifecycleAutostartSet(e.target.checked))} />Запускать Автонарезку при входе в систему</label>
    <p>Включается только этим переключателем. Откроется обычное окно, а настройка «Продолжать при запуске» определит возобновление фабрики. После перемещения программы выключи и снова включи автозапуск.</p>
    {data?.autostartError && <p className="factory-error">{data.autostartError}</p>}<p role="status">{message}</p>
  </section>;
}
