import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./styles.css";

// No browser chrome: block the default context menu and zoom shortcuts.
window.addEventListener("contextmenu", (e) => {
  const t = e.target as HTMLElement;
  if (!t.closest("input, textarea, [contenteditable=true]")) e.preventDefault();
});
window.addEventListener("keydown", (e) => {
  if (e.ctrlKey && ["+", "-", "=", "0", "p", "r"].includes(e.key.toLowerCase())) e.preventDefault();
  if (e.key === "F5") e.preventDefault();
});
window.addEventListener("wheel", (e) => e.ctrlKey && e.preventDefault(), { passive: false });

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
