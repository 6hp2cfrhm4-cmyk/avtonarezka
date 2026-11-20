import { useEffect, useRef, useState, type ReactNode } from "react";
import { DENSE_ABOVE } from "../lib/dense";

/** Фиксированная высота строк: за порогом 24 в DOM только видимые строки с небольшим запасом. */
export default function VirtualRows<T>({ items, height = 560, rowHeight = 68, render, label, virtualize }: {
  items: T[]; virtualize?: boolean; height?: number; rowHeight?: number; render: (item: T, index: number) => ReactNode; label: string;
}) {
  const [top, setTop] = useState(0);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => { if (ref.current && ref.current.scrollTop > items.length * rowHeight) ref.current.scrollTop = 0; }, [items.length, rowHeight]);
  if (!(virtualize ?? items.length > DENSE_ABOVE)) return <div role="list" aria-label={label}>{items.map((item, index) => <div role="listitem" key={index}>{render(item, index)}</div>)}</div>;
  const start = Math.max(0, Math.floor(top / rowHeight) - 3);
  const end = Math.min(items.length, Math.ceil((top + height) / rowHeight) + 3);
  return <div ref={ref} className="factory-virtual" role="list" aria-label={label} onScroll={e => setTop(e.currentTarget.scrollTop)} style={{ height }}>
    <div style={{ height: items.length * rowHeight, position: "relative" }}>
      {items.slice(start, end).map((item, offset) => <div role="listitem" key={start + offset} style={{ position: "absolute", top: (start + offset) * rowHeight, height: rowHeight, width: "100%" }}>{render(item, start + offset)}</div>)}
    </div>
  </div>;
}
