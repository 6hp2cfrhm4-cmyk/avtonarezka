import { useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { motion } from "motion/react";
import { api, errText, fmtDuration, type BannerAnim, type Draft, type DraftConfig } from "../lib/api";
import { useMediaUrl } from "../lib/useMediaUrl";
import { useUi } from "../store";
import { Chips, Color, Row, Section, Seg, Slide } from "./StyleControls";

const pct = (v: number) => `${Math.round(v * 100)}%`;
const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v));

/** Правка одного ролика: стиль, баннер, фон, субтитры, звук и границы момента. После «Пересобрать» превью обновляется. */
export default function DraftEditor({ draft, onClose }: { draft: Draft; onClose: () => void }) {
  const { assets, defaults, setDefaults, renderProgress, notify } = useUi();
  const [cfg, setCfg] = useState<DraftConfig>(() => clone(draft.config));
  const [b, setB] = useState({ start: draft.start, end: draft.end, climax: draft.climax });
  const [busy, setBusy] = useState(false);

  // другой черновик — другие настройки
  useEffect(() => {
    setCfg(clone(draft.config));
    setB({ start: draft.start, end: draft.end, climax: draft.climax });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draft.id]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const rendering = draft.status === "queued" || draft.status === "rendering";
  const video = useMediaUrl(draft.proxyPath, draft.updatedAt);
  const poster = useMediaUrl(draft.posterPath, draft.updatedAt);
  const progress = renderProgress[draft.id] ?? 0;

  const dirty = useMemo(
    () => JSON.stringify(cfg) !== JSON.stringify(draft.config) || b.start !== draft.start || b.end !== draft.end || b.climax !== draft.climax,
    [cfg, b, draft],
  );

  const style = cfg.style;
  const set = (fn: (c: DraftConfig) => void) => setCfg((c) => {
    const n = clone(c);
    fn(n);
    return n;
  });

  const banners = assets.filter((a) => a.kind === "banner");
  const backgrounds = assets.filter((a) => a.kind === "background");
  const music = assets.filter((a) => a.kind === "music");
  const bg = backgrounds.find((a) => a.id === cfg.backgroundId);
  const outLen = b.end - b.start + (style.banner.enabled && cfg.bannerId ? banners.find((x) => x.id === cfg.bannerId)?.duration ?? 0 : 0);

  const apply = async () => {
    setBusy(true);
    try {
      await api.draftUpdate(draft.id, cfg, { start: b.start, end: b.end, climax: b.climax });
      const p = useUi.getState().project;
      if (p) useUi.getState().setDrafts(await api.draftsList(p.id));
    } catch (e) {
      notify(errText(e), "error");
    } finally {
      setBusy(false);
    }
  };

  const saveDefault = async () => {
    if (!defaults) return;
    const next = { ...defaults, style: clone(cfg.style) };
    setDefaults(next);
    await api.defaultsSet(next).then(() => notify("Стиль сохранён: он будет у новых черновиков")).catch((e) => notify(errText(e), "error"));
  };

  const L = b.end - b.start;
  // В портале: родитель экрана трансформирован, и `fixed` внутри него считался бы от него, а не от окна.
  return createPortal(
    <div className="fixed inset-0 z-40 flex justify-end" style={{ perspective: 2000 }}>
      <motion.div className="absolute inset-0 bg-black/65" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} onClick={onClose} />
      <motion.aside
        initial={{ x: 120, rotateY: -22, opacity: 0 }}
        animate={{ x: 0, rotateY: 0, opacity: 1 }}
        exit={{ x: 120, rotateY: -22, opacity: 0 }}
        transition={{ type: "spring", stiffness: 190, damping: 24 }}
        style={{ transformOrigin: "right center" }}
        className="relative grid h-full w-[min(1000px,97vw)] grid-cols-[300px_1fr] gap-0 border-l border-white/10 bg-[#0b0b13]"
        // плотный фон: за панелью остаются карточки, и прозрачность мешала бы читать
      >
        {/* превью */}
        <div className="flex flex-col gap-4 border-r border-white/8 p-5">
          <div>
            <div className="font-display text-lg font-semibold">Часть {draft.part}</div>
            <div className="mt-0.5 truncate text-sm text-white/50">{draft.title}</div>
          </div>
          <div className="relative aspect-[9/16] w-full overflow-hidden rounded-2xl bg-black">
            {video && !rendering ? (
              <video key={video} src={video} poster={poster ?? undefined} controls loop muted autoPlay playsInline className="h-full w-full object-contain" />
            ) : poster && !rendering ? (
              <img src={poster} alt="" className="h-full w-full object-contain" />
            ) : (
              <div className="grid h-full place-items-center p-4 text-center text-sm text-white/45">
                {rendering ? "Собираю превью…" : draft.error ?? "Превью пока нет"}
              </div>
            )}
            {rendering && (
              <div className="absolute inset-x-4 bottom-4">
                <div className="h-1.5 overflow-hidden rounded-full bg-white/15">
                  <motion.div className="h-full bg-accent" animate={{ width: `${progress * 100}%` }} transition={{ type: "spring", stiffness: 100, damping: 20 }} />
                </div>
              </div>
            )}
          </div>
          <div className="text-xs leading-relaxed text-white/40">
            Итоговая длина: {fmtDuration(outLen)}. Во время баннера верх замирает, субтитры скрываются.
          </div>
          {draft.error && !rendering && <div className="rounded-xl border border-danger/40 bg-danger/10 p-3 text-xs text-[#ffd3db]">{draft.error}</div>}
        </div>

        {/* настройки */}
        <div className="flex min-h-0 flex-col">
          <div className="flex-1 overflow-y-auto p-5">
            <div className="flex flex-col gap-4">
              <Section title="Момент">
                <Slide label="Начало" value={b.start} min={Math.max(0, draft.start - 30)} max={b.end - 3} step={0.5} fmt={fmtDuration} onChange={(v) => setB({ ...b, start: v })} />
                <Slide label="Конец" value={b.end} min={b.start + 3} max={draft.end + 30} step={0.5} fmt={fmtDuration} onChange={(v) => setB({ ...b, end: v })} />
                <Slide label="Секунда баннера" value={b.climax} min={b.start + 0.2 * L} max={b.end - 0.1 * L} step={0.5} fmt={(v) => fmtDuration(v - b.start)} onChange={(v) => setB({ ...b, climax: v })} />
              </Section>

              <Section title="Раскладка">
                <Slide label="Верх занимает" value={style.split} min={0.4} max={0.7} fmt={pct} onChange={(v) => set((c) => void (c.style.split = v))} />
                <Seg value={style.topFit} options={[{ v: "fill", l: "Заполнить кадр" }, { v: "contain", l: "Вписать с размытием" }]} onChange={(v) => set((c) => void (c.style.topFit = v))} />
                <Slide label="Срезать снизу (вшитые субтитры исходника)" value={style.topCropBottom} min={0} max={0.4} step={0.01} fmt={pct} onChange={(v) => set((c) => void (c.style.topCropBottom = v))} />
                <Row label="Геймплей замирает вместе с верхом" hint="Выключено: фон продолжает играть, пока показывается баннер" value={style.gameplayFreeze} onChange={(v) => set((c) => void (c.style.gameplayFreeze = v))} />
              </Section>

              <Section title="Баннер">
                <Row label="Показывать баннер" value={style.banner.enabled} onChange={(v) => set((c) => void (c.style.banner.enabled = v))} />
                <Chips value={cfg.bannerId} options={[...banners.map((a) => ({ v: a.id as number | null, l: a.name })), { v: null, l: "Без баннера" }]} onChange={(v) => set((c) => void (c.bannerId = v))} />
                <Seg<BannerAnim> value={style.banner.anim} options={[{ v: "none", l: "Сразу" }, { v: "fade", l: "Плавно" }, { v: "slideUp", l: "Снизу" }, { v: "pop", l: "Pop" }]} onChange={(v) => set((c) => void (c.style.banner.anim = v))} />
                <Slide label="Ширина" value={style.banner.widthPct} min={0.3} max={1} fmt={pct} onChange={(v) => set((c) => void (c.style.banner.widthPct = v))} />
                <Slide label="Сдвиг по горизонтали" value={style.banner.offsetX} min={-0.3} max={0.3} fmt={pct} onChange={(v) => set((c) => void (c.style.banner.offsetX = v))} />
                <Slide label="Сдвиг по вертикали" value={style.banner.offsetY} min={-0.3} max={0.3} fmt={pct} onChange={(v) => set((c) => void (c.style.banner.offsetY = v))} />
                <Color label="Цвет фона баннера" value={style.banner.keyColor} onChange={(v) => set((c) => void (c.style.banner.keyColor = v))} />
                <Slide label="Чувствительность к зелёному" value={style.banner.similarity} min={0.05} max={0.4} fmt={pct} onChange={(v) => set((c) => void (c.style.banner.similarity = v))} />
                <Slide label="Мягкость края" value={style.banner.blend} min={0} max={0.3} fmt={pct} onChange={(v) => set((c) => void (c.style.banner.blend = v))} />
                <Row label="Убрать зелёный ореол по краям" value={style.banner.despill} onChange={(v) => set((c) => void (c.style.banner.despill = v))} />
              </Section>

              <Section title="Фон (геймплей)">
                <Chips value={cfg.backgroundId} options={backgrounds.map((a) => ({ v: a.id as number | null, l: a.name }))} onChange={(v) => set((c) => void (c.backgroundId = v))} />
                {bg && (
                  <>
                    <Slide label="Начать с секунды" value={cfg.gameplayOffset} min={0} max={Math.max(0, bg.duration - 5)} step={0.5} fmt={fmtDuration} onChange={(v) => set((c) => void (c.gameplayOffset = v))} />
                    <button className="btn self-start !py-1.5 !text-xs" onClick={() => set((c) => void (c.gameplayOffset = Math.round(Math.random() * Math.max(0, bg.duration - outLen - 1) * 2) / 2))}>
                      Другой кусок
                    </button>
                  </>
                )}
                {backgrounds.length === 0 && <div className="text-sm text-white/40">Добавь фон в библиотеке на шаге «Импорт»</div>}
              </Section>

              <Section title="Субтитры">
                <Row label="Показывать субтитры" value={style.subtitles.enabled} onChange={(v) => set((c) => void (c.style.subtitles.enabled = v))} />
                <Slide label="Размер" value={style.subtitles.size} min={40} max={140} step={2} onChange={(v) => set((c) => void (c.style.subtitles.size = v))} />
                <Seg value={style.subtitles.wordsPerLine} options={[2, 3, 4, 5].map((n) => ({ v: n, l: `${n} слова` }))} onChange={(v) => set((c) => void (c.style.subtitles.wordsPerLine = v))} />
                <Seg<number | null> value={style.subtitles.yPct} options={[{ v: 0.28, l: "Вверху" }, { v: null, l: "На линии раздела" }, { v: 0.82, l: "Внизу" }]} onChange={(v) => set((c) => void (c.style.subtitles.yPct = v))} />
                <Row label="ЗАГЛАВНЫМИ БУКВАМИ" value={style.subtitles.uppercase} onChange={(v) => set((c) => void (c.style.subtitles.uppercase = v))} />
                <Color label="Цвет текста" value={style.subtitles.color} onChange={(v) => set((c) => void (c.style.subtitles.color = v))} />
                <Color label="Подсветка слова" value={style.subtitles.highlight} onChange={(v) => set((c) => void (c.style.subtitles.highlight = v))} />
                <Slide label="Обводка" value={style.subtitles.outline} min={0} max={14} step={0.5} onChange={(v) => set((c) => void (c.style.subtitles.outline = v))} />
              </Section>

              <Section title="Надпись «Часть N»">
                <Row label="Показывать надпись" value={style.partLabel.enabled} onChange={(v) => set((c) => void (c.style.partLabel.enabled = v))} />
                <label className="block text-sm text-white/75">
                  Текст ({"{n}"} — номер части)
                  <input
                    className="mt-1.5 w-full rounded-xl border border-white/10 bg-white/5 px-3 py-2 text-sm outline-none focus:border-accent/60"
                    value={style.partLabel.template}
                    onChange={(e) => set((c) => void (c.style.partLabel.template = e.target.value))}
                  />
                </label>
                <Slide label="Размер" value={style.partLabel.size} min={40} max={120} step={2} onChange={(v) => set((c) => void (c.style.partLabel.size = v))} />
                <Slide label="Положение сверху" value={style.partLabel.yPct} min={0.04} max={0.3} fmt={pct} onChange={(v) => set((c) => void (c.style.partLabel.yPct = v))} />
                <Color label="Цвет" value={style.partLabel.color} onChange={(v) => set((c) => void (c.style.partLabel.color = v))} />
              </Section>

              <Section title="Звук">
                <Slide label="Голос из видео" value={style.audio.main} min={0} max={1.5} step={0.05} fmt={pct} onChange={(v) => set((c) => void (c.style.audio.main = v))} />
                <Slide label="Звук баннера" value={style.audio.banner} min={0} max={1.5} step={0.05} fmt={pct} onChange={(v) => set((c) => void (c.style.audio.banner = v))} />
                <Slide label="Звук геймплея" value={style.audio.gameplay} min={0} max={1} step={0.05} fmt={pct} onChange={(v) => set((c) => void (c.style.audio.gameplay = v))} />
                <Chips value={cfg.musicId} options={[{ v: null as number | null, l: "Без музыки" }, ...music.map((a) => ({ v: a.id as number | null, l: a.name }))]} onChange={(v) => set((c) => void (c.musicId = v))} />
                <Slide label="Громкость музыки" value={style.audio.music} min={0} max={1} step={0.02} fmt={pct} onChange={(v) => set((c) => void (c.style.audio.music = v))} />
                <Row label="Выровнять громкость (−14 LUFS)" hint="Только в финальном рендере" value={style.audio.loudnorm} onChange={(v) => set((c) => void (c.style.audio.loudnorm = v))} />
              </Section>
            </div>
          </div>

          <div className="flex items-center justify-between gap-3 border-t border-white/8 p-4">
            <div className="flex gap-2">
              <button className="btn" onClick={onClose}>Закрыть</button>
              <button className="btn" onClick={() => void saveDefault()} title="Новые черновики будут собираться с этим стилем">Сделать стилем по умолчанию</button>
            </div>
            <button className="btn btn-primary" disabled={busy || rendering || !dirty} onClick={() => void apply()}>
              {rendering ? "Собирается…" : dirty ? "Пересобрать" : "Без изменений"}
            </button>
          </div>
        </div>
      </motion.aside>
    </div>,
    document.body,
  );
}
