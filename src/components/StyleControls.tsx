import type { ReactNode } from "react";
import { Switch } from "./Controls";

export function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="rounded-2xl border border-white/8 bg-white/4 p-4">
      <div className="mb-3 font-display text-[11px] font-semibold uppercase tracking-[0.18em] text-white/45">{title}</div>
      <div className="flex flex-col gap-4">{children}</div>
    </div>
  );
}

export function Slide({
  label, value, min, max, step = 0.01, fmt, onChange, disabled,
}: { label: string; value: number; min: number; max: number; step?: number; fmt?: (v: number) => string; onChange: (v: number) => void; disabled?: boolean }) {
  return (
    <label className="block">
      <div className="mb-1.5 flex items-baseline justify-between text-sm">
        <span className="text-white/75">{label}</span>
        <span className="tabular-nums text-accent">{fmt ? fmt(value) : value}</span>
      </div>
      <input type="range" className="range" min={min} max={Math.max(max, min + step)} step={step} value={value} disabled={disabled} onChange={(e) => onChange(+e.target.value)} />
    </label>
  );
}

export function Row({ label, hint, value, onChange }: { label: string; hint?: string; value: boolean; onChange: (v: boolean) => void }) {
  return (
    <div className="flex items-center justify-between gap-4">
      <div>
        <div className="text-sm text-white/75">{label}</div>
        {hint && <div className="text-xs text-white/35">{hint}</div>}
      </div>
      <Switch checked={value} onChange={onChange} />
    </div>
  );
}

export function Seg<T extends string | number | null>({
  value, options, onChange,
}: { value: T; options: { v: T; l: string }[]; onChange: (v: T) => void }) {
  return (
    <div className="grid rounded-xl border border-white/10 bg-white/5 p-1" style={{ gridTemplateColumns: `repeat(${options.length}, 1fr)` }}>
      {options.map((o) => (
        <button
          key={String(o.v)}
          onClick={() => onChange(o.v)}
          className={`rounded-lg px-2 py-1.5 text-xs font-semibold transition-colors ${value === o.v ? "bg-accent text-accent-ink" : "text-white/60 hover:text-white"}`}
        >
          {o.l}
        </button>
      ))}
    </div>
  );
}

/** Цвет как RRGGBB без решётки — так его хранит бэкенд. */
export function Color({ label, value, onChange }: { label: string; value: string; onChange: (v: string) => void }) {
  return (
    <label className="flex items-center justify-between gap-3 text-sm text-white/75">
      {label}
      <span className="flex items-center gap-2">
        <span className="text-xs tabular-nums text-white/40">#{value}</span>
        <input
          type="color"
          value={`#${value.padEnd(6, "0").slice(0, 6)}`}
          onChange={(e) => onChange(e.target.value.slice(1))}
          className="h-8 w-10 cursor-pointer rounded-lg border border-white/15 bg-transparent p-0.5"
        />
      </span>
    </label>
  );
}

export function Chips<T extends number | null>({
  value, options, onChange,
}: { value: T; options: { v: T; l: string }[]; onChange: (v: T) => void }) {
  return (
    <div className="flex flex-wrap gap-2">
      {options.map((o) => (
        <button
          key={String(o.v)}
          onClick={() => onChange(o.v)}
          className={`chip cursor-pointer transition-colors ${value === o.v ? "!border-accent !bg-accent/15 !text-accent" : "hover:!border-white/25"}`}
        >
          {o.l}
        </button>
      ))}
    </div>
  );
}
