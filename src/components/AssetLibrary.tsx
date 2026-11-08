import { useCallback, useEffect } from "react";
import { AnimatePresence, motion } from "motion/react";
import { open } from "@tauri-apps/plugin-dialog";
import { api, errText, fmtDuration, type Asset, type AssetKind, type Defaults } from "../lib/api";
import { useMediaUrl } from "../lib/useMediaUrl";
import { useUi } from "../store";
import { Check, Trash, Upload } from "./icons";

export const VIDEO_EXT = ["mp4", "mov", "mkv", "webm", "avi", "m4v"];
export const AUDIO_EXT = ["mp3", "wav", "m4a", "aac", "ogg", "flac", "opus"];

const COLUMNS: { kind: AssetKind; title: string; hint: string; ext: string[]; filter: string }[] = [
  { kind: "banner", title: "Баннер", hint: "Видео на зелёном фоне. Играет в самый интересный момент", ext: VIDEO_EXT, filter: "Видео" },
  { kind: "background", title: "Фон", hint: "Геймплей для нижней половины. Для каждого ролика берётся случайный кусок", ext: VIDEO_EXT, filter: "Видео" },
  { kind: "music", title: "Музыка", hint: "Тихо играет под роликом", ext: AUDIO_EXT, filter: "Аудио" },
];

/** Выбор по умолчанию: что подставляется в новые черновики. */
export function isSelected(d: Defaults | null, a: Asset): boolean {
  if (!d) return false;
  return a.kind === "banner" ? d.bannerId === a.id : a.kind === "music" ? d.musicId === a.id : d.backgroundIds.includes(a.id);
}

/** Добавляет файлы в библиотеку. Первый баннер, фон и трек выбираются сами. */
export async function addAssets(kind: AssetKind, paths: string[]) {
  const ui = useUi.getState();
  for (const path of paths) {
    try {
      const a = await api.assetAdd(kind, path);
      const d = ui.defaults;
      if (d) {
        const next: Defaults = { ...d };
        if (kind === "banner" && d.bannerId === null) next.bannerId = a.id;
        if (kind === "music" && d.musicId === null) next.musicId = a.id;
        if (kind === "background" && !d.backgroundIds.includes(a.id)) next.backgroundIds = [...d.backgroundIds, a.id];
        if (next !== d) {
          await api.defaultsSet(next);
          ui.setDefaults(next);
        }
      }
    } catch (e) {
      ui.notify(errText(e), "error");
    }
  }
  ui.setAssets(await api.assetsList());
}

export default function AssetLibrary({ hoverKind }: { hoverKind: AssetKind | null }) {
  const { assets, defaults, setDefaults, setAssets, notify } = useUi();

  // Экран открыт — читаем актуальные данные: файлы могли добавить из другого места.
  useEffect(() => {
    api.assetsList().then(setAssets).catch((e) => notify(errText(e), "error"));
    api.defaultsGet().then(setDefaults).catch((e) => notify(errText(e), "error"));
  }, [setAssets, setDefaults, notify]);

  const pick = async (kind: AssetKind, ext: string[], name: string) => {
    const sel = await open({ multiple: true, filters: [{ name, extensions: ext }] });
    const paths = Array.isArray(sel) ? sel : typeof sel === "string" ? [sel] : [];
    if (paths.length) await addAssets(kind, paths);
  };

  const toggle = useCallback(
    async (a: Asset) => {
      if (!defaults) return;
      const next: Defaults = { ...defaults };
      if (a.kind === "banner") next.bannerId = defaults.bannerId === a.id ? null : a.id;
      else if (a.kind === "music") next.musicId = defaults.musicId === a.id ? null : a.id;
      else
        next.backgroundIds = defaults.backgroundIds.includes(a.id)
          ? defaults.backgroundIds.filter((x) => x !== a.id)
          : [...defaults.backgroundIds, a.id];
      setDefaults(next);
      await api.defaultsSet(next).catch((e) => notify(errText(e), "error"));
    },
    [defaults, setDefaults, notify],
  );

  const remove = async (a: Asset) => {
    await api.assetDelete(a.id).catch((e) => notify(errText(e), "error"));
    if (defaults && isSelected(defaults, a)) {
      const next: Defaults = {
        ...defaults,
        bannerId: defaults.bannerId === a.id ? null : defaults.bannerId,
        musicId: defaults.musicId === a.id ? null : defaults.musicId,
        backgroundIds: defaults.backgroundIds.filter((x) => x !== a.id),
      };
      setDefaults(next);
      await api.defaultsSet(next).catch(() => undefined);
    }
    setAssets(await api.assetsList());
  };

  return (
    <section>
      <div className="mb-3 flex items-baseline justify-between">
        <h2 className="font-display text-sm font-semibold uppercase tracking-[0.18em] text-white/45">Библиотека</h2>
        <span className="text-xs text-white/35">Перетащи файл в нужную колонку или нажми «Добавить»</span>
      </div>
      <div className="grid gap-4 lg:grid-cols-3">
        {COLUMNS.map((c) => {
          const items = assets.filter((a) => a.kind === c.kind);
          const hot = hoverKind === c.kind;
          return (
            <div
              key={c.kind}
              data-asset-kind={c.kind}
              className={`glass flex min-h-[250px] flex-col gap-3 rounded-[24px] p-4 transition-shadow ${hot ? "!border-accent" : ""}`}
              style={hot ? { boxShadow: "0 0 0 2px var(--accent), 0 0 40px -8px var(--accent)" } : undefined}
            >
              <div className="flex items-start justify-between gap-3">
                <div>
                  <div className="font-display text-base font-semibold">{c.title}</div>
                  <div className="mt-0.5 text-xs leading-snug text-white/40">{c.hint}</div>
                </div>
                <button className="btn !px-3 !py-1.5 !text-xs" onClick={() => void pick(c.kind, c.ext, c.filter)}>
                  <Upload size={14} /> Добавить
                </button>
              </div>
              <div className="flex max-h-[330px] flex-col gap-2 overflow-y-auto pr-1" style={{ perspective: 800 }}>
                <AnimatePresence initial={false}>
                  {items.map((a) => (
                    <AssetRow key={a.id} a={a} selected={isSelected(defaults, a)} onToggle={() => void toggle(a)} onRemove={() => void remove(a)} />
                  ))}
                </AnimatePresence>
                {items.length === 0 && (
                  <div className="grid flex-1 place-items-center rounded-2xl border border-dashed border-white/12 py-8 text-center text-sm text-white/35">
                    {hot ? "Отпускай!" : "Пока пусто"}
                  </div>
                )}
              </div>
            </div>
          );
        })}
      </div>
    </section>
  );
}

function AssetRow({ a, selected, onToggle, onRemove }: { a: Asset; selected: boolean; onToggle: () => void; onRemove: () => void }) {
  const thumb = useMediaUrl(a.thumb);
  return (
    <motion.div
      layout
      initial={{ opacity: 0, rotateX: -35, y: 16 }}
      animate={{ opacity: 1, rotateX: 0, y: 0 }}
      exit={{ opacity: 0, x: -40 }}
      whileHover={{ x: 4 }}
      transition={{ type: "spring", stiffness: 240, damping: 22 }}
      onClick={onToggle}
      className={`group relative flex cursor-pointer items-center gap-3 rounded-2xl border p-2 ${
        selected ? "border-accent/60 bg-accent/8" : "border-white/8 bg-white/4"
      }`}
    >
      <div className="relative h-12 w-16 shrink-0 overflow-hidden rounded-xl bg-black/50">
        {thumb ? (
          <img src={thumb} alt="" className="h-full w-full object-cover" />
        ) : (
          <div className="grid h-full place-items-center text-[10px] text-white/30">{a.kind === "music" ? "♪" : "…"}</div>
        )}
      </div>
      <div className="min-w-0 flex-1">
        <div className="truncate text-sm font-semibold">{a.name}</div>
        <div className="text-xs text-white/40">
          {fmtDuration(a.duration)}
          {a.width > 0 && ` · ${a.width}×${a.height}`}
        </div>
      </div>
      {selected && (
        <span className="grid h-6 w-6 shrink-0 place-items-center rounded-full bg-accent text-accent-ink">
          <Check size={13} />
        </span>
      )}
      <button
        onClick={(e) => {
          e.stopPropagation();
          onRemove();
        }}
        title="Убрать из библиотеки (файл останется на диске)"
        className="absolute -right-1 -top-1 grid h-6 w-6 place-items-center rounded-full bg-ink-800 text-white/40 opacity-0 transition hover:text-danger group-hover:opacity-100"
      >
        <Trash size={12} />
      </button>
    </motion.div>
  );
}
