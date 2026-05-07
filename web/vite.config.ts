/// <reference types="vitest/config" />
import { defineConfig, type Plugin } from 'vite'
import react from '@vitejs/plugin-react'
import { copyFileSync, existsSync, readFileSync } from 'fs'
import { resolve } from 'path'

// Copy VAD + ONNX runtime files to the output so they're served as static assets
function copyVadAssets(): Plugin {
  const filesToCopy = [
    ['node_modules/@ricky0123/vad-web/dist/vad.worklet.bundle.min.js', 'vad.worklet.bundle.min.js'],
    ['node_modules/@ricky0123/vad-web/dist/silero_vad_legacy.onnx', 'silero_vad_legacy.onnx'],
    ['node_modules/onnxruntime-web/dist/ort-wasm-simd-threaded.wasm', 'ort-wasm-simd-threaded.wasm'],
    ['node_modules/onnxruntime-web/dist/ort-wasm-simd-threaded.mjs', 'ort-wasm-simd-threaded.mjs'],
  ]

  return {
    name: 'copy-vad-assets',
    writeBundle(options) {
      const outDir = options.dir || resolve(__dirname, 'dist')
      for (const [src, dest] of filesToCopy) {
        const srcPath = resolve(__dirname, src)
        const destPath = resolve(outDir, dest)
        if (existsSync(srcPath)) {
          copyFileSync(srcPath, destPath)
        }
      }
    },
    configureServer(server) {
      // Serve files in dev mode too
      server.middlewares.use((req, res, next) => {
        const name = req.url?.split('?')[0]?.slice(1)
        if (name) {
          const match = filesToCopy.find(([, dest]) => dest === name)
          if (match) {
            const srcPath = resolve(__dirname, match[0])
            if (existsSync(srcPath)) {
              const ext = name.split('.').pop()
              const types: Record<string, string> = {
                wasm: 'application/wasm',
                onnx: 'application/octet-stream',
                js: 'application/javascript',
                mjs: 'application/javascript',
              }
              res.setHeader('Content-Type', types[ext || ''] || 'application/octet-stream')
              res.end(readFileSync(srcPath))
              return
            }
          }
        }
        next()
      })
    },
  }
}

export default defineConfig({
  plugins: [react(), copyVadAssets()],
  build: {
    chunkSizeWarningLimit: 1100,
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (id.includes('@git-diff-view/')) return 'git-diff-vendor'
          if (id.includes('react-markdown') || id.includes('remark-gfm')) return 'markdown-vendor'
        },
      },
    },
  },
  server: {
    host: true,
    allowedHosts: true,
    watch: {
      usePolling: true,
      interval: 120,
    },
    proxy: {
      '/api': `http://localhost:${process.env.VITE_API_PORT ?? '4200'}`,
      '/ws': {
        target: `ws://localhost:${process.env.VITE_API_PORT ?? '4200'}`,
        ws: true,
      },
    },
  },
  test: {
    environment: 'jsdom',
    setupFiles: './src/test-setup.ts',
    globals: true,
  },
})
