import { useEffect, useState } from "react";
import { api, errText, type ModelsErrorEvent, type ModelsProgressEvent, type ModelsStatus } from "../lib/api";
import { onEvent } from "../lib/events";
import { useUi } from "../store";

/** Полоса наверху: докачка моделей при запуске, ошибка загрузки или кнопка, если моделей не хватает. Когда всё на месте, её нет. */
export default function ModelsBanner() {
  const notify = useUi((s) => s.notify);
  const [status, setStatus] = useState<ModelsStatus | null>(null);
  const [progress, setProgress] = useState<ModelsProgressEvent | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = () => api.modelsStatus().then(setStatus).catch((e) => notify(errText(e), "error"));

  useEffect(() => {
    void reload();
    const offs = [
      onEvent<ModelsProgressEvent>("models-progress", (p) => {
        setProgress(p);
        setError(null);
      }),
      onEvent("models-done", () => {
        setProgress(null);
        setError(null);
        void reload();
      }),
      onEvent<ModelsErrorEvent>("models-error", (e) => {
        setProgress(null);
        setError(e.message);
        void reload();
      }),
    ];
    return () => offs.forEach((f) => f());
  }, []);

  if (!status || (status.ready && !status.downloading && !error)) return null;

  const start = () => void api.modelsDownload().then(reload).catch((e) => notify(errText(e), "error"));
  const pct = progress && progress.total ? Math.min(100, Math.round((progress.received / progress.total) * 100)) : 0;
  const fileName = progress?.name.split("/").pop();

  return (
    <div className="flex flex-wrap items-center justify-between gap-3 rounded-2xl border border-white/10 bg-white/[0.04] px-5 py-3 text-sm">
      {status.downloading || progress ? (
        <div className="min-w-0 flex-1">
          <div className="text-white/80">
            Скачиваю модели для анализа{fileName ? `: ${fileName}, ${pct}%` : "…"}
          </div>
          <div className="mt-2 h-1.5 w-full overflow-hidden rounded-full bg-white/10">
            <div className="h-full rounded-full bg-accent transition-[width]" style={{ width: `${pct}%` }} />
          </div>
        </div>
      ) : error ? (
        <div className="min-w-0 flex-1 text-[#ffd3db]">Модели не скачались: {error}</div>
      ) : (
        <div className="min-w-0 flex-1 text-white/70">Для анализа нужны модели, около 3 ГБ. Не все есть на диске, загрузка сейчас не идёт.</div>
      )}
      {!status.downloading && (
        <button className="btn !py-1.5 !text-xs" onClick={start}>
          {error ? "Повторить" : "Скачать модели"}
        </button>
      )}
    </div>
  );
}
