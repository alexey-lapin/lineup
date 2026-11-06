import './assets/main.css'
import './icons'
import { createApp } from 'vue'
import { createPinia } from 'pinia'
import { PiniaColada } from '@pinia/colada'
import ui from '@nuxt/ui/vue-plugin'
import App from './App.vue'
import { router } from './router'
import { watchSafeArea } from './safe-area'

const app = createApp(App)
app.use(createPinia())
app.use(PiniaColada, { queryOptions: { staleTime: 5_000 } })
app.use(router)
app.use(ui)
app.mount('#app')
watchSafeArea()

// Service workers need a secure context (HTTPS or localhost); over plain HTTP on a LAN the app
// still works from the home screen, just without offline support.
if (import.meta.env.PROD && 'serviceWorker' in navigator && window.isSecureContext) {
  navigator.serviceWorker.register('/sw.js').catch((e) => console.warn('service worker:', e))
}
