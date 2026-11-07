import { useQuery, useQueryCache } from '@pinia/colada'
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useToast } from '@nuxt/ui/composables'
import { api, ApiError } from './api'

export function useChannels() {
  return useQuery({ key: ['channels'], query: () => api.channels() })
}

export function usePlaylists() {
  return useQuery({ key: ['playlists'], query: () => api.playlists() })
}

/** Most mutations change channels and playlist contents together; refresh all of them. */
export function useInvalidateAll() {
  const cache = useQueryCache()
  return () =>
    // Refetch failures are surfaced by the queries themselves; never reject the caller.
    Promise.allSettled([
      cache.invalidateQueries({ key: ['channels'] }),
      cache.invalidateQueries({ key: ['playlists'] }),
      cache.invalidateQueries({ key: ['playlist'] }),
      cache.invalidateQueries({ key: ['status'] }),
      cache.invalidateQueries({ key: ['settings'] }),
      cache.invalidateQueries({ key: ['integrations'] })
    ])
}

export type ActionResult<T> = { ok: true; value: T } | { ok: false }

/**
 * Run an action and toast its error. Success is reported explicitly (not by the value), because
 * 204 responses legitimately resolve to `undefined`.
 */
export function useAction() {
  const toast = useToast()
  return async <T>(fn: () => Promise<T>, success?: string): Promise<ActionResult<T>> => {
    try {
      const value = await fn()
      if (success) toast.add({ title: success, color: 'success', icon: 'i-lucide-check' })
      return { ok: true, value }
    } catch (e) {
      const msg = e instanceof ApiError ? e.message : String(e)
      toast.add({ title: 'Error', description: msg, color: 'error', icon: 'i-lucide-circle-x' })
      return { ok: false }
    }
  }
}

export async function copy(text: string, toast: ReturnType<typeof useToast>) {
  try {
    await navigator.clipboard.writeText(text)
    toast.add({ title: 'Copied', description: text, icon: 'i-lucide-clipboard-check' })
  } catch {
    // Clipboard API needs a secure context; fall back to a prompt the user can copy from.
    window.prompt('Copy this URL', text)
  }
}

/** Deep links into iOS players (they accept http stream URLs). */
export function playerLinks(url: string) {
  const e = encodeURIComponent(url)
  return {
    vlc: `vlc-x-callback://x-callback-url/stream?url=${e}`,
    infuse: `infuse://x-callback-url/play?url=${e}`
  }
}

/**
 * Keep queries fresh from the server's event stream (`/api/v1/events`). On (re)connecting, or when
 * the server says this client missed events, everything is refetched.
 */
export function useLiveUpdates() {
  const cache = useQueryCache()
  const refetch = (...keys: string[]) =>
    Promise.allSettled(keys.map((k) => cache.invalidateQueries({ key: [k] })))
  const everything = () =>
    refetch('channels', 'playlists', 'playlist', 'epg-sources', 'hooks', 'settings', 'status', 'integrations')
  let es: EventSource | undefined
  onMounted(() => {
    if (typeof EventSource === 'undefined') return
    es = new EventSource('/api/v1/events')
    // Refetch on every (re)connect, the first included: a change between the initial queries and
    // the subscription would otherwise go unnoticed.
    es.addEventListener('ready', everything)
    es.addEventListener('resync', everything)
    es.addEventListener('playlist.changed', () => refetch('channels', 'playlists', 'playlist'))
    es.addEventListener('epg.updated', () => refetch('epg-sources'))
    es.addEventListener('settings.changed', () => refetch('settings'))
    // URL changes also come as playlist.changed; this covers the rest (schedule, acexy for checks).
    es.addEventListener('integration.changed', () => refetch('integrations', 'channels'))
    for (const e of ['channel.unhealthy', 'channel.recovered', 'channel.probed']) {
      es.addEventListener(e, () => refetch('channels', 'playlist'))
    }
  })
  onBeforeUnmount(() => es?.close())
}

/**
 * Poll `fetch(key)` every `everyMs` while the component is mounted and the page is visible. A
 * request never overlaps the previous one, and an answer for an older key (or after unmounting) is
 * dropped. `key` returning undefined pauses polling; a new key clears the old data and polls now.
 */
export function usePolling<K, T>(key: () => K | undefined, fetch: (key: K) => Promise<T>, everyMs: number) {
  const data = ref<T>()
  const error = ref('')
  const loading = ref(false)
  let timer: ReturnType<typeof setTimeout> | undefined
  let seq = 0
  let active = false

  const visible = () => typeof document === 'undefined' || !document.hidden

  async function tick() {
    clearTimeout(timer)
    timer = undefined
    const k = key()
    if (k === undefined || !active || !visible()) return
    const mine = ++seq
    loading.value = true
    try {
      const v = await fetch(k)
      if (mine === seq) {
        data.value = v
        error.value = ''
      }
    } catch (e) {
      if (mine === seq) error.value = e instanceof ApiError ? e.message : String(e)
    } finally {
      if (mine === seq) {
        loading.value = false
        if (active && visible()) timer = setTimeout(tick, everyMs)
      }
    }
  }

  const onVisibility = () => (visible() ? void tick() : clearTimeout(timer))
  watch(key, () => {
    seq++ // drop answers for the old key
    data.value = undefined
    error.value = ''
    loading.value = false
    void tick()
  })
  onMounted(() => {
    active = true
    document.addEventListener('visibilitychange', onVisibility)
    void tick()
  })
  onBeforeUnmount(() => {
    active = false
    seq++
    clearTimeout(timer)
    document.removeEventListener('visibilitychange', onVisibility)
  })
  return { data, error, loading, refresh: tick }
}
