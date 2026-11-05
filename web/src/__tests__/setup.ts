import { vi } from 'vitest'
import { createPinia } from 'pinia'
import { PiniaColada } from '@pinia/colada'
import { createMemoryHistory, createRouter } from 'vue-router'

export const toasts: unknown[] = []
vi.mock('@nuxt/ui/composables', () => ({ useToast: () => ({ add: (t: unknown) => toasts.push(t) }) }))

export function deferred<T>() {
  let resolve!: (v: T) => void
  let reject!: (e: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

export function plugins() {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', component: { template: '<div/>' } },
      { path: '/playlists', component: { template: '<div/>' } },
      { path: '/playlists/:slug', component: { template: '<div/>' } }
    ]
  })
  return { router, list: [createPinia(), PiniaColada, router] }
}

export const flush = () => new Promise((r) => setTimeout(r, 0))
