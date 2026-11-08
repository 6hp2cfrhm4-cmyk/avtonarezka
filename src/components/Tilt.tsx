import { useEffect, useRef, type HTMLAttributes, type ReactNode } from "react";

type TiltProps = { children: ReactNode; strength?: number } & HTMLAttributes<HTMLDivElement>;

/**
 * Наклон за указателем. Никаких пружин и состояния React: на движение мыши пишутся две CSS-переменные,
 * а сам transform и плавность (transition) описаны в `.tilt` (styles.css). Работает только у карточки под курсором,
 * у остальных нет ни слоя, ни анимации, поэтому сотня карточек стоит как сотня обычных блоков.
 */
export default function Tilt({ children, strength = 10, className = "", ...rest }: TiltProps) {
  const ref = useRef<HTMLDivElement>(null);
  const frame = useRef(0);
  useEffect(() => () => cancelAnimationFrame(frame.current), []);

  const set = (rx: number, ry: number) => {
    cancelAnimationFrame(frame.current);
    frame.current = requestAnimationFrame(() => {
      ref.current?.style.setProperty("--rx", `${rx}deg`);
      ref.current?.style.setProperty("--ry", `${ry}deg`);
    });
  };

  return (
    <div className={className} {...rest}>
      <div
        ref={ref}
        className="tilt h-full w-full"
        onPointerMove={(e) => {
          const r = e.currentTarget.getBoundingClientRect();
          const x = (e.clientX - r.left) / r.width - 0.5;
          const y = (e.clientY - r.top) / r.height - 0.5;
          set(-2 * strength * y, 2 * strength * x);
        }}
        onPointerLeave={() => set(0, 0)}
      >
        {children}
      </div>
    </div>
  );
}
