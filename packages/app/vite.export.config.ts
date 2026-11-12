// Build the standalone /export page for the static vcad.io site:
//   npx vite build -c vite.export.config.ts   → packages/app/dist-export
// www/vite.config.js copies the result to /export/ in the landing deploy.
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "url";
import { dirname, resolve } from "path";

const __dirname = dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  root: resolve(__dirname, "export-site"),
  base: "/export/",
  publicDir: false,
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": resolve(__dirname, "./src") } },
  build: { outDir: resolve(__dirname, "dist-export"), emptyOutDir: true },
});
