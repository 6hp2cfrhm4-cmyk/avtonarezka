import type { ReactNode } from "react";
import { useCallback, useEffect, useState } from "react";
import { errText } from "../lib/api";
import { onEvent } from "../lib/events";

export const factoryInput = "factory-input";
export function Field({ label, children, hint }: { label: string; children: ReactNode; hint?: string }) {
  return <label className="factory-field"><span>{label}</span>{children}{hint && <small>{hint}</small>}</label>;
}
export function FactoryHeading({ title, detail, children }: { title: string; detail: string; children?: ReactNode }) {
  return <header className="factory-heading"><div><h1>{title}</h1><p>{detail}</p></div>{children}</header>;
}
export function useFactoryData<T>(read: () => Promise<T>) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState("");
  const refresh = useCallback(async () => { try { const next = await read(); setData(next); setError(""); } catch (e) { setError(errText(e)); } }, [read]);
  useEffect(() => { void refresh(); return onEvent("factory-changed", () => void refresh()); }, [refresh]);
  return { data, error, refresh };
}
export function LoadError({ error, retry }: { error: string; retry: () => void }) {
  return error ? <div role="alert" className="factory-error">{error} <button type="button" onClick={retry}>Повторить</button></div> : null;
}
