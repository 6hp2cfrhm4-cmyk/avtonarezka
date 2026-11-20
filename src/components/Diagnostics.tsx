import { useCallback, useEffect, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { api, errText, type DiagnosticBundle } from "../lib/api";
import VirtualRows from "./VirtualRows";

const levels: Record<string, string> = { error: "Ошибка", warn: "Предупреждение", info: "Событие", debug: "Отладка", trace: "Трассировка" };
const components: Record<string, string> = { analysis: "Анализ", render: "Рендер", publish: "Публикации", sources: "Источники", factory: "Фабрика", operations: "Эксплуатация", app: "Приложение", dependency: "Библиотека" };
const classes: Record<string, string> = { memory: "Недостаточно памяти", gpu: "Видеокарта", authorization: "Авторизация", quota: "Квота", captcha: "Капча", network: "Соединение", missing_file: "Файл не найден", cancelled: "Остановлено", other: "Общее событие" };
export default function Diagnostics() {
  const [data, setData] = useState<DiagnosticBundle | null>(null); const [busy, setBusy] = useState(false); const [message, setMessage] = useState("");
  const inspect = useCallback(async () => { setBusy(true); setMessage(""); try { setData(await api.diagnosticsCollect()); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } }, []);
  useEffect(() => { void inspect(); }, [inspect]);
  const exportBundle = async () => { setBusy(true); setMessage(""); try { const path = await save({ defaultPath: `avtonarezka-diagnostics-${new Date().toISOString().slice(0, 10)}.json`, filters: [{ name: "Диагностика JSON", extensions: ["json"] }] }); if (!path) return; await api.diagnosticsExport(path); setMessage("Пакет диагностики сохранён"); } catch (e) { setMessage(errText(e)); } finally { setBusy(false); } };
  return <section className="factory-panel" aria-label="Диагностика фабрики"><div className="factory-section-heading"><h2>Диагностика и журналы</h2><div className="factory-actions"><button className="factory-secondary" disabled={busy} onClick={() => void inspect()}>{busy ? "Подготавливаю…" : "Проверить состояние"}</button><button className="factory-secondary" disabled={busy} onClick={() => void exportBundle()}>Сохранить диагностику</button></div></div>
    <p>JSON-пакет включает версии, состояния очередей, ресурсы и последние события. Тексты ошибок заменены категорией и отпечатком. Секреты, подписи, имена файлов и адреса источников в пакет не входят.</p><p role="status">{message}</p>
    {data && <><dl className="diagnostics-summary"><div><dt>Версия приложения</dt><dd>{data.app.version} · {data.app.os} · {data.app.arch}</dd></div><div><dt>База данных</dt><dd>Схема {data.database.schema} · ошибок связей: {data.database.foreignKeyViolations}</dd></div><div><dt>Текущая работа</dt><dd>Анализ: {data.resources.analyses} · рендер: {data.resources.renders} · загрузки: {data.resources.downloads}</dd></div><div><dt>Публикации, требующие проверки</dt><dd>{data.database.publicationStates.needs_check ?? 0}</dd></div><div><dt>Собрано</dt><dd>{new Date(data.createdAt).toLocaleString("ru-RU")}</dd></div></dl>
      {data.logWriteFailed && <p className="factory-error">Структурированный журнал не удалось сохранить на диск. Проверь доступ и свободное место.</p>}<h3>Последние события приложения</h3><p>Подробные сообщения фабрики доступны на главном экране в журнале «Что происходит».</p>
      <VirtualRows items={data.runtimeLog.slice().reverse().map((r, id) => ({ ...r, id }))} rowHeight={64} label="Структурированный журнал приложения" render={r => <div className="diagnostics-row"><span><strong>{levels[r.level] ?? r.level} · {components[r.component] ?? r.component}</strong><small>{classes[r.class] ?? r.class}</small></span><time dateTime={new Date(r.at * 1000).toISOString()}>{new Date(r.at * 1000).toLocaleString("ru-RU")}</time></div>} />{!data.runtimeLog.length && <p className="factory-empty">Событий этого запуска пока нет.</p>}</>}
  </section>;
}
