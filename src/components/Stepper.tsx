import { motion } from "motion/react";
import { Check, Lock } from "./icons";
import { maxStep, SCREENS, useUi } from "../store";

export default function Stepper() {
  const screen = useUi((s) => s.screen);
  const project = useUi((s) => s.project);
  const go = useUi((s) => s.go);
  const reach = maxStep(project);
  const active = SCREENS.findIndex((s) => s.id === screen);

  return (
    <nav className="relative flex flex-col gap-2" style={{ perspective: 900 }}>
      <div className="absolute left-[22px] top-6 bottom-6 w-px bg-white/10" />
      <motion.div
        className="absolute left-[22px] top-6 w-px bg-accent"
        animate={{ height: `calc(${(active / (SCREENS.length - 1)) * 100}% - ${(active / (SCREENS.length - 1)) * 48}px)` }}
        transition={{ type: "spring", stiffness: 120, damping: 22 }}
        style={{ boxShadow: "0 0 12px var(--accent)" }}
      />
      {SCREENS.map((s, i) => {
        const locked = i > reach;
        const done = i < active;
        const isActive = i === active;
        return (
          <motion.button
            key={s.id}
            disabled={locked}
            onClick={() => go(s.id)}
            whileHover={locked ? undefined : { x: 6, rotateY: -8, z: 14 }}
            animate={{ x: isActive ? 10 : 0, rotateY: isActive ? -6 : 0, z: isActive ? 24 : 0 }}
            transition={{ type: "spring", stiffness: 260, damping: 20 }}
            style={{ transformStyle: "preserve-3d", transformOrigin: "left center" }}
            className={`relative z-10 flex items-center gap-3 rounded-2xl px-2 py-2.5 text-left outline-none ${
              locked ? "cursor-not-allowed opacity-40" : "cursor-pointer"
            } ${isActive ? "glass" : ""}`}
          >
            <span
              className={`grid h-9 w-9 shrink-0 place-items-center rounded-xl border text-sm font-bold transition-colors ${
                isActive
                  ? "border-transparent bg-accent text-accent-ink"
                  : done
                    ? "border-accent/40 bg-accent/10 text-accent"
                    : "border-white/10 bg-ink-800 text-white/60"
              }`}
              style={isActive ? { boxShadow: "0 0 22px -2px var(--accent)" } : undefined}
            >
              {isActive ? i + 1 : done ? <Check size={16} /> : locked ? <Lock size={14} /> : i + 1}
            </span>
            <span className="min-w-0">
              <span className="block font-display text-[13px] font-semibold tracking-wide">{s.label}</span>
              <span className="block truncate text-xs text-white/45">{s.hint}</span>
            </span>
          </motion.button>
        );
      })}
    </nav>
  );
}
