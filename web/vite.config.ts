import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import ui from '@nuxt/ui/vite'
import type { Plugin } from 'vite'

const server = process.env.LINEUP_DEV_SERVER ?? 'http://127.0.0.1:9300'

/** Writes shell.json, the list of every built asset, which the service worker caches for offline use. */
function shellList(): Plugin {
  return {
    name: 'lineup-shell-list',
    apply: 'build',
    generateBundle(_, bundle) {
      const assets = Object.keys(bundle)
        .filter((f) => f.startsWith('assets/'))
        .map((f) => `/${f}`)
        .sort()
      this.emitFile({ type: 'asset', fileName: 'shell.json', source: JSON.stringify({ assets }) })
    }
  }
}

export default defineConfig({
  plugins: [
    shellList(),
    vue(),
    ui({
      ui: {
        colors: { primary: 'emerald', neutral: 'zinc' }
      }
    })
  ],
  server: {
    proxy: {
      '/api': server,
      '/p/': server
    }
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true
  }
})
