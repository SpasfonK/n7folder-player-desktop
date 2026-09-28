import { defineConfig } from "vite";

// WebView2 (Chromium) : on peut cibler un navigateur récent, donc pas de transpilation lourde.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**", "**/crates/**", "**/target/**"] },
  },
  build: {
    target: "chrome105",
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
  },
});
