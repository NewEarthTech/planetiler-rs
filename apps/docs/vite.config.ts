import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  // Served under planetiler.rs/benchmarks (infra/docs deploys dist/ to that
  // key prefix), so every asset URL has to carry the prefix too.
  base: "/benchmarks/",
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { "@": path.resolve(import.meta.dirname, "src") },
  },
});
