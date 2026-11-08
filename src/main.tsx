import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";
import App from "./App";
import { useUi } from "./store";

// Только в dev: позволяет из консоли браузера переключать экраны и состояние (в сборке этой строки нет).
if (import.meta.env.DEV) (window as unknown as { __ui: typeof useUi }).__ui = useUi;

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
