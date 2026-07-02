import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri v2 dev server config. Fixed port so the Rust side can point WKWebView at it.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  envPrefix: ["VITE_", "TAURI_"],
  server: {
    port: 1420,
    strictPort: true,
    host: false,
  },
  build: {
    target: "esnext",
  },
});
