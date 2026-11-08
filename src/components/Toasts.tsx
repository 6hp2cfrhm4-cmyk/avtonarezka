import { AnimatePresence, motion } from "motion/react";
import { useUi } from "../store";

export default function Toasts() {
  const toasts = useUi((s) => s.toasts);
  return (
    <div className="pointer-events-none fixed bottom-6 right-6 z-50 flex w-[380px] flex-col gap-2">
      <AnimatePresence>
        {toasts.map((t) => (
          <motion.div
            key={t.id}
            layout
            initial={{ opacity: 0, x: 80, rotateY: 40, scale: 0.9 }}
            animate={{ opacity: 1, x: 0, rotateY: 0, scale: 1 }}
            exit={{ opacity: 0, x: 80, rotateY: 40, scale: 0.9 }}
            transition={{ type: "spring", stiffness: 300, damping: 24 }}
            className={`glass-lite pointer-events-auto rounded-2xl px-4 py-3 text-sm leading-snug ${
              t.kind === "error" ? "border-danger/50 text-[#ffd3db]" : ""
            }`}
            style={{ transformOrigin: "right center" }}
          >
            {t.text}
            {t.count > 1 && <span className="ml-2 rounded-md bg-white/10 px-1.5 py-0.5 text-xs font-bold tabular-nums">×{t.count}</span>}
          </motion.div>
        ))}
      </AnimatePresence>
    </div>
  );
}
