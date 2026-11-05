import { defineConfig } from 'vitest/config'
import vue from '@vitejs/plugin-vue'

// Components are mounted shallowly (Nuxt UI children stubbed), so the Nuxt UI plugin is not needed.
export default defineConfig({
  plugins: [vue()],
  test: { environment: 'happy-dom' }
})
