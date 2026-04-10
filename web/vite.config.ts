import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    host: '0.0.0.0',
    allowedHosts: ['joshs-mac-mini-2.tailbcc5a.ts.net'],
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:7422',
        changeOrigin: true,
        ws: true,
      },
    },
  },
})
