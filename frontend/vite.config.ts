import ui from '@codex-proxy/ui/vite'
import tailwind from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

export default defineConfig({
  base: './',
  define: { 'process.env.NODE_ENV': JSON.stringify('production') },
  plugins: [vue(), tailwind(), ui(), {
    name: 'plugin-classic-entry',
    generateBundle() {
      // 宿主隔离页只接受经典脚本，不能直接交付 Vite 的 module 入口。
      // CPR 3.37 起宿主按 meta 声明的前缀注入主题变量（缺省 z）；本页 UI 库读取 --cp-*，必须声明 cp。
      this.emitFile({ type: 'asset', fileName: 'index.html', source: '<!doctype html><html lang="zh-CN"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="codex-proxy-ui-theme-prefix" content="cp"><link rel="stylesheet" href="./app.css"></head><body><div id="app"></div><script defer src="./app.js"></script></body></html>' })
    },
  }],
  build: {
    lib: { entry: 'src/main.ts', name: 'KeyResetFollow', formats: ['iife'], fileName: () => 'app.js', cssFileName: 'app' },
    cssCodeSplit: false,
  },
})
