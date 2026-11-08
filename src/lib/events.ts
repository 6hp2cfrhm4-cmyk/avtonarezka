import { listen } from "@tauri-apps/api/event";
import { inTauri, mockBus } from "./api";

/** Подписка на событие бэкенда. В обычном браузере (npm run dev) события приходят от заглушки. */
export function onEvent<T>(name: string, cb: (payload: T) => void): () => void {
  if (!inTauri) {
    const h = (e: Event) => cb((e as CustomEvent<T>).detail);
    mockBus.addEventListener(name, h);
    return () => mockBus.removeEventListener(name, h);
  }
  let off: (() => void) | undefined;
  let dead = false;
  listen<T>(name, (e) => cb(e.payload)).then((f) => (dead ? f() : (off = f)));
  return () => {
    dead = true;
    off?.();
  };
}
