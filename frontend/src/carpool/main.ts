import {
  applyResolvedTheme,
  DEFAULT_CUSTOM_THEME_COLOR,
  DEFAULT_THEME_COLOR,
  resolveTheme,
} from '@codex-proxy/ui/theme'
import { createApp } from 'vue'
import CarpoolApp from './CarpoolApp.vue'
import '@codex-proxy/ui/styles.css'
import './style.css'

function syncTheme() {
  const isDark = typeof window !== 'undefined'
    && window.matchMedia
    && window.matchMedia('(prefers-color-scheme: dark)').matches
  const theme = resolveTheme(
    isDark ? 'dark' : 'light',
    DEFAULT_THEME_COLOR,
    DEFAULT_CUSTOM_THEME_COLOR,
  )
  applyResolvedTheme(document.documentElement, theme)
}

if (typeof window !== 'undefined') {
  syncTheme()
  window
    .matchMedia('(prefers-color-scheme: dark)')
    .addEventListener('change', syncTheme)
}

createApp(CarpoolApp).mount('#app')
