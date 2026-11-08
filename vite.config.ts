import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: "127.0.0.1",
    // windows/ держит кэш Wine на десятки тысяч файлов: без исключения Vite упирается в лимит наблюдателей и память
    watch: { ignored: ["**/src-tauri/**", "**/windows/**", "**/release/**", "**/dist/**"] },
  },
  build: { target: "safari15", chunkSizeWarningLimit: 1500 },
});
