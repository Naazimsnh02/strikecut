import { defineConfig } from "vite";

// Tauri expects a fixed dev port and a build that targets its webviews.
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: { target: "es2022", outDir: "dist" },
});
