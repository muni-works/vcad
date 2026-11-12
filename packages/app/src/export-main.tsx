// Standalone /export entry for the static vcad.io site (www/). Renders only
// the read-only export off-ramp — no editor, no kernel WASM. Built by
// vite.export.config.ts into dist-export/, which www's build copies to
// /export/. The full app's main.tsx still routes /export the same way.
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { ExportAllPage } from "./components/ExportAllPage";
import "./export.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ExportAllPage />
  </StrictMode>,
);
