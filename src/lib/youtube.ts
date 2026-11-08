import type { EnvReport } from "./api";

/** Версия yt-dlp, с которой работает утилита-образец; на более старой YouTube чаще отвечает ошибками. */
export const MIN_YTDLP = "2026.10.26";

/** Как поставить yt-dlp с Deno: на Linux пакетом, на Windows они уже лежат в папке tools рядом с программой. */
export const installYtdlpHint = (os?: string) =>
  os === "windows" ? "они входят в установку (папка tools рядом с программой), переустанови Автонарезку" : "sudo pacman -S yt-dlp deno";

/** Как обновить yt-dlp. */
export const updateYtdlpHint = (os?: string) =>
  os === "windows" ? "yt-dlp.exe -U в папке tools рядом с программой" : "sudo pacman -Syu yt-dlp";

/** Первая ссылка на YouTube в произвольном тексте (из буфера обмена бывает «посмотри: https://youtu.be/…»). */
export function extractYoutubeLink(text: string): string | null {
  const m = text.match(/(?:https?:\/\/)?(?:(?:www|m|music)\.)?(?:youtube\.com|youtu\.be)\/[^\s<>"']+/i);
  // «…смотри https://youtu.be/abc, оно огонь»: запятая в конце фразы не часть ссылки
  return m ? m[0].replace(/[.,;:!?)\]}»]+$/, "") : null;
}

const UNITS = ["Б", "КБ", "МБ", "ГБ", "ТБ"];

/** Размер в байтах человеческим языком; пустое значение — «—», ничего не выдумываем. */
export function fmtBytes(n: number | null | undefined): string {
  if (n == null || n <= 0) return "—";
  let v = n;
  let i = 0;
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024;
    i++;
  }
  return `${i === 0 ? Math.round(v) : v >= 100 ? Math.round(v) : v.toFixed(1).replace(".", ",")} ${UNITS[i]}`;
}

export function fmtSpeed(bytesPerSec: number | null | undefined): string {
  return bytesPerSec ? `${fmtBytes(bytesPerSec)}/с` : "—";
}

export function fmtEta(sec: number | null | undefined): string {
  if (sec == null || sec < 0) return "—";
  const s = Math.round(sec);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${String(r).padStart(2, "0")}` : `${String(m).padStart(2, "0")}:${String(r).padStart(2, "0")}`;
}

/** «4K», «1440p», «1080p» по высоте кадра. */
export function resLabel(height: number): string {
  if (height >= 4320) return "8K";
  if (height >= 2160) return "4K";
  return `${height}p`;
}

export const PRESET_LABEL: Record<string, string> = { "720": "720p", "1080": "1080p", "1440": "1440p", "2160": "4K", best: "Лучшее" };

/**
 * К ошибке yt-dlp добавляется подсказка, если он старее нужной версии: чаще всего дело в этом.
 * Наши собственные сообщения по-русски (про ссылку, папку), у yt-dlp они английские, по ним и различаем.
 */
export function withVersionHint(message: string, env: EnvReport | undefined): string {
  if (/^[А-Яа-яЁё]/.test(message)) return message;
  if (env?.ytdlp && !env.ytdlpOk) {
    return `${message}\nВерсия yt-dlp (${env.ytdlp}) старше ${MIN_YTDLP}. Если ошибка не уходит, обнови её: ${updateYtdlpHint(env.os)}.`;
  }
  return message;
}
