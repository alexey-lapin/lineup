import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import ui from '@nuxt/ui/vite'

const server = process.env.LINEUP_DEV_SERVER ?? 'http://127.0.0.1:9300'

export default defineConfig({
  plugins: [
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
