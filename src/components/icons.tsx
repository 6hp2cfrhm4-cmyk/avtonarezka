import type { SVGProps } from "react";

type P = SVGProps<SVGSVGElement> & { size?: number };

const base = (size = 18): SVGProps<SVGSVGElement> => ({
  width: size,
  height: size,
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 2.2,
  strokeLinecap: "round",
  strokeLinejoin: "round",
});

export const Check = ({ size, ...p }: P) => (
  <svg {...base(size)} {...p}>
    <path d="M5 12.5l4.5 4.5L19 7.5" />
  </svg>
);
export const Lock = ({ size, ...p }: P) => (
  <svg {...base(size)} {...p}>
    <rect x="5" y="11" width="14" height="9" rx="2.5" />
    <path d="M8 11V8a4 4 0 018 0v3" />
  </svg>
);
export const Upload = ({ size, ...p }: P) => (
  <svg {...base(size)} {...p}>
    <path d="M12 16V4M7 9l5-5 5 5" />
    <path d="M4 15v3a2 2 0 002 2h12a2 2 0 002-2v-3" />
  </svg>
);
export const Arrow = ({ size, ...p }: P) => (
  <svg {...base(size)} {...p}>
    <path d="M5 12h14M13 6l6 6-6 6" />
  </svg>
);
export const Trash = ({ size, ...p }: P) => (
  <svg {...base(size)} {...p}>
    <path d="M4 7h16M10 11v6M14 11v6M6 7l1 12a2 2 0 002 2h6a2 2 0 002-2l1-12M9 7V4h6v3" />
  </svg>
);
export const Film = ({ size, ...p }: P) => (
  <svg {...base(size)} {...p}>
    <rect x="3" y="4" width="18" height="16" rx="3" />
    <path d="M8 4v16M16 4v16M3 9h5M3 15h5M16 9h5M16 15h5" />
  </svg>
);
export const Link = ({ size, ...p }: P) => (
  <svg {...base(size)} {...p}>
    <path d="M10 14a4 4 0 005.7 0l3-3a4 4 0 00-5.7-5.7l-1 1" />
    <path d="M14 10a4 4 0 00-5.7 0l-3 3a4 4 0 005.7 5.7l1-1" />
  </svg>
);
export const Download = ({ size, ...p }: P) => (
  <svg {...base(size)} {...p}>
    <path d="M12 4v12M7 11l5 5 5-5" />
    <path d="M4 15v3a2 2 0 002 2h12a2 2 0 002-2v-3" />
  </svg>
);
export const Close = ({ size, ...p }: P) => (
  <svg {...base(size)} {...p}>
    <path d="M6 6l12 12M18 6L6 18" />
  </svg>
);
export const Stop = ({ size, ...p }: P) => (
  <svg {...base(size)} {...p}>
    <rect x="6.5" y="6.5" width="11" height="11" rx="2.5" />
  </svg>
);
export const Sliders =({ size, ...p }: P) => (
  <svg {...base(size)} {...p}>
    <path d="M4 7h10M18 7h2M4 17h2M10 17h10" />
    <circle cx="16" cy="7" r="2" />
    <circle cx="8" cy="17" r="2" />
  </svg>
);
