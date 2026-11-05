import './assets/main.css'
import './icons'
import { createApp } from 'vue'
import { createPinia } from 'pinia'
import { PiniaColada } from '@pinia/colada'
import ui from '@nuxt/ui/vue-plugin'
import App from './App.vue'
import { router } from './router'

const app = createApp(App)
app.use(createPinia())
app.use(PiniaColada, { queryOptions: { staleTime: 5_000 } })
app.use(router)
app.use(ui)
app.mount('#app')
