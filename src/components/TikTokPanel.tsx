import { useEffect, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { api, errText, type LoginEvent, type Privacy, type TikTokMethod, type TikTokMode, type TikTokStatus } from "../lib/api";
import { onEvent } from "../lib/events";
import { useUi } from "../store";
import { Row, Seg } from "./StyleControls";

const PRIVACY_LABEL: Record<Privacy, string> = {
  SELF_ONLY: "Только я",
  MUTUAL_FOLLOW_FRIENDS: "Друзья",
  FOLLOWER_OF_CREATOR: "Подписчики",
  PUBLIC_TO_EVERYONE: "Все",
};
export const privacyLabel = (p: Privacy) => PRIVACY_LABEL[p];

const input = "w-full rounded-xl border border-white/10 bg-white/5 px-3 py-2 text-sm outline-none transition focus:border-accent/60";

export default function TikTokPanel() {
  const { tiktok, setTikTok, notify } = useUi();
  const [form, setForm] = useState<{ key: string; secret: string; port: string; mode: TikTokMode; method: TikTokMethod; privacy: Privacy; auto: boolean } | null>(null);
  const [busy, setBusy] = useState(false);
  const [login, setLogin] = useState<string | null>(null);
  const [help, setHelp] = useState(false);

  // форма берёт значения один раз; дальше правит пользователь
  useEffect(() => {
    if (tiktok && !form) {
      setForm({ key: tiktok.clientKey, secret: "", port: String(tiktok.port), mode: tiktok.mode, method: tiktok.method, privacy: tiktok.privacy, auto: tiktok.autoPublish });
    }
  }, [tiktok, form]);

  useEffect(() => {
    api.tiktokStatus().then(setTikTok).catch((e) => notify(errText(e), "error"));
  }, [setTikTok, notify]);

  // Подпись «идёт вход» живёт, пока не придёт итог: успех, ошибка или отмена.
  useEffect(() => {
    if (tiktok?.loginInProgress) setLogin((l) => l ?? "Подтверди доступ в открывшемся браузере");
  }, [tiktok?.loginInProgress]);

  useEffect(
    () => onEvent<LoginEvent>("tiktok-login", (e) => setLogin(e.state === "waiting" ? e.message : null)),
    [],
  );

  if (!tiktok || !form) return <div className="glass rounded-2xl p-5 text-sm text-white/50">Загружаю настройки TikTok…</div>;

  const set = <K extends keyof NonNullable<typeof form>>(k: K, v: NonNullable<typeof form>[K]) => setForm({ ...form, [k]: v });
  const changedAuth = form.key.trim() !== tiktok.clientKey || form.mode !== tiktok.mode || form.secret !== "";
  const apiMode = form.method === "api";

  const save = async (): Promise<TikTokStatus | null> => {
    setBusy(true);
    try {
      const s = await api.tiktokSaveSettings({
        clientKey: form.key.trim(),
        clientSecret: form.secret || undefined,
        port: Number(form.port) || 0,
        mode: form.mode,
        method: form.method,
        privacy: form.privacy,
        autoPublish: form.auto,
      });
      setTikTok(s);
      set("secret", "");
      notify("Настройки TikTok сохранены");
      return s;
    } catch (e) {
      notify(errText(e), "error");
      return null;
    } finally {
      setBusy(false);
    }
  };

  const connect = async () => {
    // сначала сохраняем: вход использует сохранённые ключи
    if (changedAuth || Number(form.port) !== tiktok.port) {
      if (!(await save())) return;
    }
    try {
      setLogin("Открываю браузер…");
      await api.tiktokLoginStart();
    } catch (e) {
      setLogin(null);
      notify(errText(e), "error");
    }
  };

  const copy = (text: string) => navigator.clipboard?.writeText(text).then(() => notify("Скопировано")).catch(() => notify("Не удалось скопировать", "error"));
  const until = tiktok.refreshExpiresAt ? new Date(tiktok.refreshExpiresAt * 1000).toLocaleDateString("ru-RU") : null;
  const ready = form.key.trim() !== "" && (tiktok.hasSecret || form.secret !== "");

  return (
    <section className="glass flex flex-col gap-5 rounded-2xl p-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h2 className="font-display text-sm font-semibold uppercase tracking-[0.18em] text-white/55">TikTok</h2>
        <span className={`chip ${apiMode && tiktok.connected ? "!border-accent/40 !text-accent" : ""}`}>
          {apiMode ? (tiktok.connected ? `Подключён до ${until}` : "Не подключён") : tiktok.chromium ? "Через браузер" : "Нужен Chromium"}
        </span>
      </div>

      <Seg<TikTokMethod>
        value={form.method}
        options={[{ v: "api", l: "Официальный API" }, { v: "browser", l: "Через браузер" }]}
        onChange={(v) => set("method", v)}
      />

      {apiMode ? (
        <>
          <div>
            <Seg<TikTokMode>
              value={form.mode}
              options={[{ v: "inbox", l: "В черновики TikTok" }, { v: "direct", l: "Опубликовать сразу" }]}
              onChange={(v) => set("mode", v)}
            />
            <p className="mt-2 text-xs leading-relaxed text-white/45">
              {form.mode === "inbox"
                ? "Ролик попадает в черновики приложения TikTok. Подпись и публикация — там же: программа подпись не передаёт, зато её можно скопировать."
                : "Ролик публикуется сразу. Пока приложение не прошло аудит TikTok, публиковать можно только с видимостью «Только я»."}
            </p>
          </div>

          {form.mode === "direct" && (
            <div>
              <div className="mb-1.5 text-sm text-white/75">Видимость по умолчанию</div>
              <Seg<Privacy>
                value={form.privacy}
                options={(Object.keys(PRIVACY_LABEL) as Privacy[]).map((p) => ({ v: p, l: PRIVACY_LABEL[p] }))}
                onChange={(v) => set("privacy", v)}
              />
            </div>
          )}

          <div className="grid gap-3 sm:grid-cols-2">
            <label className="block text-sm text-white/75">
              Client key
              <input className={`${input} mt-1.5`} value={form.key} onChange={(e) => set("key", e.target.value)} placeholder="из кабинета TikTok" spellCheck={false} autoComplete="off" />
            </label>
            <label className="block text-sm text-white/75">
              Client secret
              <input
                className={`${input} mt-1.5`}
                type="password"
                value={form.secret}
                onChange={(e) => set("secret", e.target.value)}
                placeholder={tiktok.hasSecret ? "••••••••  сохранён" : "из кабинета TikTok"}
                spellCheck={false}
                autoComplete="new-password"
              />
            </label>
          </div>

          <label className="block text-sm text-white/75">
            Порт для входа
            <input className={`${input} mt-1.5 !w-32`} inputMode="numeric" value={form.port} onChange={(e) => set("port", e.target.value.replace(/\D/g, ""))} />
          </label>

          {tiktok.secretsBackend === "memory" && (
            <div className="rounded-xl border border-[#ffb020]/30 bg-[#ffb020]/8 p-3 text-xs leading-relaxed text-[#ffd88a]">
              В системе недоступно хранилище ключей (на Linux это secret-tool), поэтому секрет и вход хранятся только пока открыта программа.
            </div>
          )}

          <button className="self-start text-xs text-accent underline-offset-4 hover:underline" onClick={() => setHelp(!help)}>
            {help ? "Скрыть" : "Как получить ключи и что указать в кабинете"}
          </button>
          <AnimatePresence initial={false}>
            {help && (
              <motion.ol
                initial={{ height: 0, opacity: 0 }}
                animate={{ height: "auto", opacity: 1 }}
                exit={{ height: 0, opacity: 0 }}
                className="list-decimal space-y-2 overflow-hidden pl-5 text-xs leading-relaxed text-white/60"
              >
                <li>Зайди на developers.tiktok.com и создай приложение.</li>
                <li>Добавь продукты <b>Login Kit</b> и <b>Content Posting API</b>. Для черновиков нужно разрешение <b>video.upload</b>, для публикации — <b>video.publish</b>.</li>
                <li>
                  В Login Kit добавь адрес возврата ровно такой:
                  <span className="mt-1 flex items-center gap-2">
                    <code className="select-text rounded-lg bg-black/40 px-2 py-1 text-[11px] text-white/85">{`http://127.0.0.1:${form.port || tiktok.port}/callback/`}</code>
                    <button className="chip cursor-pointer" onClick={() => copy(`http://127.0.0.1:${form.port || tiktok.port}/callback/`)}>Копировать</button>
                  </span>
                </li>
                <li>Скопируй Client key и Client secret сюда. Секрет хранится в связке ключей системы, а не в базе программы.</li>
                <li>Пока приложение не прошло аудит, TikTok разрешает публиковать только «Только я», а в черновики — не больше 5 роликов за сутки.</li>
              </motion.ol>
            )}
          </AnimatePresence>

          <div className="flex flex-wrap items-center gap-2">
            {tiktok.loginInProgress || login ? (
              <>
                <span className="text-sm text-white/70">{login ?? "Жду подтверждения в браузере…"}</span>
                <button className="btn !py-1.5 !text-xs" onClick={() => void api.tiktokLoginCancel()}>Отменить</button>
              </>
            ) : (
              <>
                <button className="btn btn-primary" disabled={busy || !ready} onClick={() => void connect()}>
                  {tiktok.connected && !changedAuth ? "Подключить заново" : "Подключить аккаунт"}
                </button>
                <button className="btn" disabled={busy} onClick={() => void save()}>Сохранить</button>
                {tiktok.connected && (
                  <button className="btn" onClick={() => void api.tiktokLogout().then(setTikTok)}>Отключить</button>
                )}
                {tiktok.hasSecret && (
                  <button className="btn !text-danger" onClick={() => void api.tiktokForgetSecret().then(setTikTok).then(() => notify("Секрет удалён из связки ключей"))}>Удалить секрет</button>
                )}
              </>
            )}
          </div>
          {changedAuth && tiktok.connected && <div className="text-xs text-white/40">Смена ключа, секрета или режима отключит текущий вход: подключиться нужно будет заново.</div>}
        </>
      ) : (
        <>
          <p className="text-xs leading-relaxed text-white/50">
            Программа откроет Chromium с отдельным профилем и заполнит страницу загрузки TikTok. Войди в аккаунт один раз прямо в этом окне: пароль программа не видит.
            Способ зависит от вёрстки сайта и может перестать работать после его обновления; к тому же автоматизация может не понравиться TikTok.
          </p>
          {!tiktok.chromium && <div className="rounded-xl border border-danger/40 bg-danger/10 p-3 text-xs text-[#ffd3db]">Chromium не найден. Установи пакет chromium.</div>}
          <Row label="Нажимать «Опубликовать» самой программе" hint="Выключено: программа загрузит файл и вставит подпись, а кнопку ты нажмёшь сам, проверив результат" value={form.auto} onChange={(v) => set("auto", v)} />
          <button className="btn self-start" disabled={busy} onClick={() => void save()}>Сохранить</button>
        </>
      )}
    </section>
  );
}
