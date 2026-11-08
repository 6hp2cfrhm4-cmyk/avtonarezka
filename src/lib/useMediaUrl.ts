import { useEffect, useState } from "react";
import { api } from "./api";

const cache = new Map<string, Promise<string>>();

/** Один и тот же файл регистрируется на медиа-сервере один раз. */
export function mediaUrl(path: string): Promise<string> {
  let p = cache.get(path);
  if (!p) {
    p = api.mediaUrl(path);
    cache.set(path, p);
  }
  return p;
}

/** URL файла для `<video>`/`<img>`. `version` меняют, когда файл перезаписан, чтобы обойти кэш. */
export function useMediaUrl(path: string | null | undefined, version = 0): string | null {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let dead = false;
    if (!path) {
      setUrl(null);
      return;
    }
    mediaUrl(path)
      .then((u) => !dead && setUrl(u ? (version ? `${u}&v=${version}` : u) : null))
      .catch(() => !dead && setUrl(null));
    return () => {
      dead = true;
    };
  }, [path, version]);
  return url;
}
