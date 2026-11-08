// Подпись для TikTok собирается так же, как в Rust (`tiktok::api::make_caption`): текст, с новой строки хэштеги.
// Длина — `String.length`, то есть единицы UTF-16: так считает и бэкенд, и счётчик TikTok.

/** Хэштеги в единый вид `#тег`: без пробелов внутри, без пустых и повторов (как `normalize_hashtags`). */
export function normalizeTags(tags: string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const t of tags) {
    const w = t.trim().replace(/^#+/, "").replace(/\s+/g, "");
    if (!w || seen.has(w.toLowerCase())) continue;
    seen.add(w.toLowerCase());
    out.push(`#${w}`);
  }
  return out;
}

/** Из строки, которую правит человек («юмор, #тикток сериал»), в список слов. */
export function parseTags(text: string): string[] {
  return text.split(/[\s,]+/).map((x) => x.replace(/^#+/, "")).filter(Boolean);
}

export function composeCaption(caption: string, tags: string[]): string {
  const t = normalizeTags(tags).join(" ");
  const b = caption.trim();
  return b && t ? `${b}\n${t}` : b || t;
}

export const captionLen = (caption: string, tags: string[]) => composeCaption(caption, tags).length;
