import ui from '@codex-proxy/ui/vite'
import tailwind from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

export default defineConfig({
  base: './',
  define: { 'process.env.NODE_ENV': JSON.stringify('production') },
  plugins: [
    vue(),
    tailwind(),
    ui(),
    {
      name: 'carpool-classic-entry',
      generateBundle() {
        this.emitFile({
          type: 'asset',
          fileName: 'index.html',
          source: '<!doctype html><html lang="zh-CN"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>拼车用量与重置卡</title><link rel="stylesheet" href="./carpool.css"></head><body><div id="app"></div><script defer src="./carpool.js"></script></body></html>',
        })
      },
    },
  ],
  build: {
    outDir: 'dist/carpool',
    emptyOutDir: true,
    lib: {
      entry: 'src/carpool/main.ts',
      name: 'KeyResetCarpool',
      formats: ['iife'],
      fileName: () => 'carpool.js',
      cssFileName: 'carpool',
    },
    cssCodeSplit: false,
  },
})
