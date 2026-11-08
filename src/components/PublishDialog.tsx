import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { motion } from "motion/react";
import { api, errText, type Draft, type Privacy } from "../lib/api";
import { useUi } from "../store";
import { privacyLabel } from "./TikTokPanel";
import { Row, Seg } from "./StyleControls";

const parseTags = (t: string) => t.split(/[\s,]+/).map((x) => x.replace(/^#+/, "")).filter(Boolean);
const joinCaption = (caption: string, tags: string[]) => [caption.trim(), ...tags.map((t) => `#${t}`)].filter(Boolean).join(" ");
const basename = (p: string | null) => p?.split(/[\\/]/).pop() ?? "";

/** Подтверждение перед отправкой в TikTok: что именно уйдёт и куда. Без клика «Отправить» ничего не происходит. */
export default function PublishDialog({ draft, onClose, onOpenSettings }: { draft: Draft; onClose: () => void; onOpenSettings: () => void }) {
  const { tiktok, notify, project, setPosts } = useUi();
  const [caption, setCaption] = useState(draft.caption);
  const [tags, setTags] = useState(draft.hashtags.join(" "));
  const [privacy, setPrivacy] = useState<Privacy>(tiktok?.privacy ?? "SELF_ONLY");
  const [comment, setComment] = useState(false);
  const [duet, setDuet] = useState(false);
  const [stitch, setStitch] = useState(false);
  const [busy, setBusy] = useState(false);
  const [already, setAlready] = useState<string | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  if (!tiktok) return null;
  const viaApi = tiktok.method === "api";
  const direct = viaApi && tiktok.mode === "direct";
  const inbox = viaApi && tiktok.mode === "inbox";
  const notReady = viaApi ? !tiktok.connected : !tiktok.chromium;
  const full = joinCaption(caption, parseTags(tags));

  const copy = () => navigator.clipboard?.writeText(full).then(() => notify("Подпись скопирована")).catch(() => notify("Не удалось скопировать", "error"));

  const send = async (force: boolean) => {
    setBusy(true);
    try {
      const r = await api.tiktokPublish({ draftId: draft.id, caption, hashtags: parseTags(tags), privacy, allowComment: comment, allowDuet: duet, allowStitch: stitch, force });
      if (r.alreadySent) {
        setAlready(r.alreadySent);
        return;
      }
      if (project) setPosts(await api.postsList(project.id));
      notify("Отправка в TikTok запущена");
      onClose();
    } catch (e) {
      notify(errText(e), "error");
    } finally {
      setBusy(false);
    }
  };

  return createPortal(
    <div className="fixed inset-0 z-50 grid place-items-center p-6" style={{ perspective: 1600 }}>
      <motion.div className="absolute inset-0 bg-black/70" initial={{ opacity: 0 }} animate={{ opacity: 1 }} onClick={onClose} />
      <motion.div
        initial={{ opacity: 0, rotateX: 22, y: 60, scale: 0.94 }}
        animate={{ opacity: 1, rotateX: 0, y: 0, scale: 1 }}
        transition={{ type: "spring", stiffness: 190, damping: 22 }}
        className="relative flex max-h-[90vh] w-[min(560px,100%)] flex-col gap-4 overflow-y-auto rounded-[26px] border border-white/10 bg-[#0e0e18] p-6"
      >
        <div>
          <div className="font-display text-xl font-extrabold">Отправить в TikTok</div>
          <div className="mt-1 truncate text-sm text-white/50">Часть {draft.part} · {basename(draft.finalPath)}</div>
        </div>

        {already ? (
          <>
            <div className="rounded-2xl border border-[#ffb020]/30 bg-[#ffb020]/8 p-4 text-sm leading-relaxed text-[#ffd88a]">
              Этот ролик уже отправлялся: {already}
              <div className="mt-2 text-white/70">Отправить ещё раз? В TikTok появится второй такой же ролик.</div>
            </div>
            <div className="flex justify-end gap-2">
              <button className="btn" onClick={onClose}>Не отправлять</button>
              <button className="btn btn-primary" disabled={busy} onClick={() => void send(true)}>Всё равно отправить</button>
            </div>
          </>
        ) : notReady ? (
          <>
            <div className="rounded-2xl border border-danger/40 bg-danger/10 p-4 text-sm text-[#ffd3db]">
              {viaApi ? "Аккаунт TikTok не подключён. Укажи ключи приложения и войди в настройках." : "Для этого способа нужен Chromium."}
            </div>
            <div className="flex justify-end gap-2">
              <button className="btn" onClick={onClose}>Закрыть</button>
              <button className="btn btn-primary" onClick={() => { onOpenSettings(); onClose(); }}>Открыть настройки TikTok</button>
            </div>
          </>
        ) : (
          <>
            <label className="block text-sm text-white/75">
              Подпись
              <textarea
                className="mt-1.5 h-24 w-full resize-none rounded-xl border border-white/10 bg-white/5 px-3 py-2 text-sm outline-none focus:border-accent/60"
                value={caption}
                maxLength={2000}
                onChange={(e) => setCaption(e.target.value)}
                placeholder="Текст под роликом"
              />
            </label>
            <label className="block text-sm text-white/75">
              Хэштеги
              <input className="mt-1.5 w-full rounded-xl border border-white/10 bg-white/5 px-3 py-2 text-sm outline-none focus:border-accent/60" value={tags} onChange={(e) => setTags(e.target.value)} placeholder="юмор тикток" />
            </label>

            {direct && (
              <>
                <div>
                  <div className="mb-1.5 text-sm text-white/75">Кто увидит</div>
                  <Seg<Privacy> value={privacy} options={(["SELF_ONLY", "MUTUAL_FOLLOW_FRIENDS", "FOLLOWER_OF_CREATOR", "PUBLIC_TO_EVERYONE"] as Privacy[]).map((p) => ({ v: p, l: privacyLabel(p) }))} onChange={setPrivacy} />
                </div>
                <div className="flex flex-col gap-3">
                  <Row label="Разрешить комментарии" value={comment} onChange={setComment} />
                  <Row label="Разрешить дуэты" value={duet} onChange={setDuet} />
                  <Row label="Разрешить склейки" value={stitch} onChange={setStitch} />
                </div>
              </>
            )}

            {inbox && (
              <div className="rounded-2xl border border-white/10 bg-white/5 p-4 text-xs leading-relaxed text-white/60">
                В черновики TikTok уходит только видео. Подпись вставь в приложении TikTok сам.
                <div className="mt-3">
                  <button className="btn !py-1.5 !text-xs" onClick={copy}>Скопировать подпись с хэштегами</button>
                </div>
              </div>
            )}
            {!viaApi && (
              <div className="rounded-2xl border border-white/10 bg-white/5 p-4 text-xs leading-relaxed text-white/60">
                Откроется окно Chromium со страницей загрузки TikTok. Если попросит, войди в аккаунт в этом окне.
                {tiktok.autoPublish ? " Программа сама нажмёт «Опубликовать»." : " Кнопку «Опубликовать» нажмёшь ты, проверив результат."}
              </div>
            )}

            <div className="flex items-center justify-between gap-3 rounded-2xl bg-black/30 px-4 py-3 text-xs text-white/55">
              <span>
                Куда: {viaApi ? (inbox ? "черновики TikTok" : `публикация · ${privacyLabel(privacy)}`) : "страница загрузки TikTok"}
              </span>
              <span className="tabular-nums">{full.length} символов</span>
            </div>

            <div className="flex justify-end gap-2">
              <button className="btn" onClick={onClose}>Отмена</button>
              <button className="btn btn-primary" disabled={busy} onClick={() => void send(false)}>{busy ? "Запускаю…" : "Отправить в TikTok"}</button>
            </div>
          </>
        )}
      </motion.div>
    </div>,
    document.body,
  );
}
