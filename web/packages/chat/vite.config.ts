import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { copyFileSync, existsSync, readFileSync } from "fs";
import { resolve } from "path";

const vadFiles = [
  ["node_modules/@ricky0123/vad-web/dist/vad.worklet.bundle.min.js", "vad.worklet.bundle.min.js"],
  ["node_modules/@ricky0123/vad-web/dist/silero_vad_legacy.onnx", "silero_vad_legacy.onnx"],
  ["node_modules/onnxruntime-web/dist/ort-wasm-simd-threaded.wasm", "ort-wasm-simd-threaded.wasm"],
  ["node_modules/onnxruntime-web/dist/ort-wasm-simd-threaded.mjs", "ort-wasm-simd-threaded.mjs"],
] as const;

export default defineConfig({
  plugins: [
    react(),
    {
      name: "copy-vad-assets",
      writeBundle(options) {
        const outDir = options.dir || resolve(__dirname, "demo-dist");
        for (const [src, dest] of vadFiles) {
          const srcPath = resolve(__dirname, "../../", src);
          const destPath = resolve(outDir, dest);
          if (existsSync(srcPath)) copyFileSync(srcPath, destPath);
        }
      },
      configureServer(server) {
        server.middlewares.use((req, res, next) => {
          const name = req.url?.split("?")[0]?.slice(1);
          if (name) {
            const match = vadFiles.find(([, dest]) => dest === name);
            if (match) {
              const srcPath = resolve(__dirname, "../../", match[0]);
              if (existsSync(srcPath)) {
                const ext = name.split(".").pop();
                const types: Record<string, string> = {
                  wasm: "application/wasm",
                  onnx: "application/octet-stream",
                  js: "application/javascript",
                  mjs: "application/javascript",
                };
                res.setHeader("Content-Type", types[ext || ""] || "application/octet-stream");
                res.end(readFileSync(srcPath));
                return;
              }
            }
          }
          next();
        });
      },
    },
  ],
  resolve: {
    alias: {
      "@apiari/types": resolve(__dirname, "../types/src/index.ts"),
      "@apiari/api": resolve(__dirname, "../api/src/index.ts"),
    },
  },
  server: {
    host: "0.0.0.0",
    proxy: {
      "/api": `http://localhost:${process.env.VITE_API_PORT ?? "4200"}`,
      "/ws": { target: `ws://localhost:${process.env.VITE_API_PORT ?? "4200"}`, ws: true },
    },
  },
});
