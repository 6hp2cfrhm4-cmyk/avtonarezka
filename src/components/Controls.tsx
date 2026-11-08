import { motion } from "motion/react";
import type { ReactNode } from "react";

export function Switch({
  checked,
  onChange,
  disabled,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <button
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={`relative h-6 w-11 shrink-0 rounded-full border transition-colors ${
        checked ? "border-transparent bg-accent" : "border-white/15 bg-white/10"
      } ${disabled ? "cursor-not-allowed opacity-40" : "cursor-pointer"}`}
    >
      <motion.span
        layout
        transition={{ type: "spring", stiffness: 500, damping: 30 }}
        className={`absolute top-[3px] h-4 w-4 rounded-full ${checked ? "right-[3px] bg-accent-ink" : "left-[3px] bg-white/70"}`}
      />
    </button>
  );
}

export function Field({ label, hint, value, children }: { label: string; hint?: string; value?: ReactNode; children: ReactNode }) {
  return (
    <div>
      <div className="mb-2 flex items-baseline justify-between gap-3">
        <span className="text-sm font-semibold">{label}</span>
        {value !== undefined && <span className="text-sm tabular-nums text-accent">{value}</span>}
      </div>
      {children}
      {hint && <div className="mt-1.5 text-xs text-white/40">{hint}</div>}
    </div>
  );
}
