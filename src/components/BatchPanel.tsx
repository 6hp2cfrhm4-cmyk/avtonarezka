import { useCallback, useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { api, errText, type BatchItem, type BatchStage } from "../lib/api";
import { extractYoutubeLink } from "../lib/youtube";
import { onEvent } from "../lib/events";
import { useUi } from "../store";
import { Switch } from "./Controls";
import { Film, Link, Trash, Upload } from "./icons";

const input =
  "select-text w-full rounded-xl border border-white/10 bg-white/5 px-3 py-2.5 text-sm outline-none transition focus:border-accent/60";
const VIDEO = ["mp4", "mov", "mkv", "webm", "avi", "m4v", "ts", "mpg", "mpeg"];

const STAGE_LABEL: Record<BatchStage, string> = {
  queued: "В очереди",
  downloading: "Скачивается",
  importing: "Добавляю в проекты",
  analyzing: "Ищу моменты",
  drafting: "Делаю черновики",
  awaiting_approval: "Ждёт одобрения",
  rendering: "Рендерится финал",
  done: "Готово",
  failed: "Ошибка",
};

/** Качество для скачивания: значение, которое понимает бэкенд, и подпись. Как в карточке YouTube. */
const PRESETS: [string, string][] = [
  ["1080", "1080p (рекомендуется)"],
  ["720", "720p"],
  ["best", "Лучшее доступное"],
];

/** Стадии, в которых пачка уже что-то делает: такой элемент нельзя убрать. */
const BUSY: BatchStage[] = ["downloading", "importing", "analyzing", "drafting"];

const card = "rounded-2xl border border-white/10 bg-white/[0.04]";

/**
 * Пачка: вставь много ссылок или добавь файлы, нажми «Запустить» — дальше каждый ролик проходит
 * скачивание, поиск моментов и черновики сам. Черновики одобряешь ты, финал запускается после одобрения.
 */
export default function BatchPanel() {
  const notify = useUi((s) => s.notify);
  const [items, setItems] = useState<BatchItem[]>([]);
  const [links, setLinks] = useState("");
  const [preset, setPreset] = useState("1080");
  const [files, setFiles] = useState<{ path: string; precut: boolean }[]>([]);
  const [busy, setBusy] = useState(false);

  const reload = useCallback(() => {
    api.batchList().then(setItems).catch((e) => notify(errText(e), "error"));
  }, [notify]);

  useEffect(() => {
    reload();
    return onEvent("batch-changed", reload);
  }, [reload]);

  const addLinks = async () => {
    const lines = links.split("\n").map((l) => l.trim()).filter(Boolean);
    const found = lines.map(extractYoutubeLink).filter((u): u is string => !!u);
    const skipped = lines.length - found.length;
    if (found.length === 0) {
      notify("В поле нет ни одной ссылки YouTube", "error");
      return;
    }
    setBusy(true);
    try {
      await api.batchAdd(found.map((url) => ({ kind: "link", source: url, preset })));
      setLinks("");
      notify(skipped > 0 ? `Добавлено ссылок: ${found.length}, пропущено строк: ${skipped}` : `Добавлено ссылок: ${found.length}`);
      reload();
    } catch (e) {
      notify(errText(e), "error");
    } finally {
      setBusy(false);
    }
  };

  const pickFiles = async () => {
    try {
      const sel = await open({ multiple: true, filters: [{ name: "Видео", extensions: VIDEO }] });
      if (!sel) return;
      const paths = Array.isArray(sel) ? sel : [sel];
      setFiles((cur) => [...cur, ...paths.filter((p) => !cur.some((f) => f.path === p)).map((path) => ({ path, precut: false }))]);
    } catch (e) {
      notify(errText(e), "error");
    }
  };

  const addFiles = async () => {
    setBusy(true);
    try {
      await api.batchAdd(files.map((f) => ({ kind: "file", source: f.path, precut: f.precut })));
      setFiles([]);
      notify(`Файлов в пачку: ${files.length}`);
      reload();
    } catch (e) {
      notify(errText(e), "error");
    } finally {
      setBusy(false);
    }
  };

  const start = async () => {
    try {
      await api.batchStart();
    } catch (e) {
      notify(errText(e), "error");
    }
  };

  const cancel = async () => {
    try {
      await api.batchCancel();
    } catch (e) {
      notify(errText(e), "error");
    }
  };

  const act = async (fn: () => Promise<unknown>) => {
    try {
      await fn();
    } catch (e) {
      notify(errText(e), "error");
    }
  };

  const running = items.some((i) => BUSY.includes(i.stage));
  const queued = items.some((i) => i.stage === "queued");
  const awaiting = items.filter((i) => i.stage === "awaiting_approval").length;

  return (
    <div className="flex flex-col gap-4">
      <section className={`${card} flex flex-col gap-3 p-5`}>
        <div className="flex items-center gap-2 text-sm font-semibold text-white/80">
          <Link size={16} /> Ссылки YouTube (по одной в строке)
        </div>
        <textarea
          value={links}
          onChange={(e) => setLinks(e.target.value)}
          rows={4}
          placeholder={"https://youtu.be/...\nhttps://www.youtube.com/watch?v=..."}
          className={`${input} font-mono text-xs`}
        />
        <div className="flex flex-wrap items-center justify-between gap-3">
          <label className="flex items-center gap-2 text-sm text-white/60">
            Качество
            <select value={preset} onChange={(e) => setPreset(e.target.value)} className="rounded-xl border border-white/10 bg-white/5 px-2 py-1.5 text-sm outline-none">
              {PRESETS.map(([value, label]) => (
                <option key={value} value={value} className="bg-[#14101f]">
                  {label}
                </option>
              ))}
            </select>
          </label>
          <button className="btn" disabled={busy || !links.trim()} onClick={() => void addLinks()}>
            Добавить ссылки
          </button>
        </div>
      </section>

      <section className={`${card} flex flex-col gap-3 p-5`}>
        <div className="flex items-center justify-between gap-3">
          <div className="flex items-center gap-2 text-sm font-semibold text-white/80">
            <Upload size={16} /> Файлы с компьютера
          </div>
          <button className="btn" onClick={() => void pickFiles()}>
            Выбрать файлы
          </button>
        </div>
        {files.length > 0 && (
          <ul className="flex flex-col gap-2">
            {files.map((f) => (
              <li key={f.path} className="flex items-center justify-between gap-3 rounded-xl bg-white/[0.03] px-3 py-2 text-sm">
                <span className="min-w-0 truncate text-white/75" title={f.path}>
                  {f.path.split(/[\\/]/).pop()}
                </span>
                <div className="flex shrink-0 items-center gap-3">
                  <label className="flex items-center gap-2 text-xs text-white/55">
                    уже нарезан
                    <Switch checked={f.precut} onChange={(v) => setFiles((cur) => cur.map((x) => (x.path === f.path ? { ...x, precut: v } : x)))} />
                  </label>
                  <button
                    className="text-white/40 hover:text-danger"
                    title="Убрать из списка"
                    onClick={() => setFiles((cur) => cur.filter((x) => x.path !== f.path))}
                  >
                    <Trash size={14} />
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
        {files.length > 0 && (
          <div className="flex justify-end">
            <button className="btn" disabled={busy} onClick={() => void addFiles()}>
              Добавить файлы в пачку ({files.length})
            </button>
          </div>
        )}
      </section>

      <section className={`${card} flex flex-col gap-3 p-5`}>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-2 text-sm font-semibold text-white/80">
            <Film size={16} /> Пачка ({items.length})
          </div>
          <div className="flex flex-wrap gap-2">
            {running ? (
              <button className="btn" onClick={() => void cancel()}>
                Остановить
              </button>
            ) : (
              <button className="btn btn-primary" disabled={!queued} onClick={() => void start()}>
                Запустить пачку
              </button>
            )}
            {awaiting > 0 && (
              <button className="btn" onClick={() => void act(async () => notify(`Одобрено черновиков: ${await api.batchApproveReady()}`))}>
                Одобрить готовые по пачке ({awaiting})
              </button>
            )}
          </div>
        </div>

        {items.length === 0 ? (
          <p className="text-sm text-white/45">Пачка пуста. Вставь ссылки или выбери файлы выше.</p>
        ) : (
          <ul className="flex flex-col gap-2">
            {items.map((it) => (
              <li key={it.id} className="flex flex-wrap items-center justify-between gap-3 rounded-xl bg-white/[0.03] px-3 py-2.5">
                <div className="min-w-0 flex-1">
                  <div className="truncate text-sm text-white/85" title={it.source}>
                    {it.title || it.source}
                  </div>
                  <div className="mt-0.5 flex flex-wrap items-center gap-2 text-xs">
                    <span className={it.stage === "failed" ? "text-danger" : it.stage === "done" ? "text-accent" : "text-white/55"}>
                      {STAGE_LABEL[it.stage]}
                    </span>
                    {it.precut && <span className="text-white/40">· уже нарезан</span>}
                    {it.error && <span className={it.stage === "failed" ? "text-danger" : "text-[#ffd88a]"}>· {it.error}</span>}
                  </div>
                </div>
                <div className="flex shrink-0 items-center gap-2">
                  {it.stage === "failed" && (
                    <button className="btn" onClick={() => void act(() => api.batchRetry(it.id))}>
                      Повторить
                    </button>
                  )}
                  {!BUSY.includes(it.stage) && (
                    <button className="text-white/40 hover:text-danger" title="Убрать из пачки" onClick={() => void act(() => api.batchRemove(it.id))}>
                      <Trash size={14} />
                    </button>
                  )}
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
