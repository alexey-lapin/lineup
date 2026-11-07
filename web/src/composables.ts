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
    const reload = () => {
      liveEpoch.value++
      return everything()
    }
    es.addEventListener('ready', reload)
    es.addEventListener('resync', reload)
    es.addEventListener('playlist.changed', () => refetch('channels', 'playlists', 'playlist'))
    es.addEventListener('epg.updated', () => refetch('epg-sources'))
    es.addEventListener('settings.changed', () => refetch('settings'))
    // URL changes also come as playlist.changed; this covers the rest (schedule, acexy for checks).
    es.addEventListener('integration.changed', () => {
      liveEpoch.value++
      return refetch('integrations', 'channels')
    })
    for (const e of ['channel.unhealthy', 'channel.recovered', 'channel.probed']) {
      es.addEventListener(e, () => refetch('channels', 'playlist'))
    }
  })
  onBeforeUnmount(() => es?.close())
}

/**
 * Bumped when the event stream (re)connects, resyncs, or an integration changes: views that read
 * live service data outside the query cache reload on it.
 */
export const liveEpoch = ref(0)

/**
 * Poll `fetch(key, signal)` every `everyMs` while the component is mounted and the page is visible.
 * One request at a time: `refresh()` during a request runs once more right after it. Hiding the
 * page, a new key, or unmounting aborts the request in flight and drops its answer. `key`
 * returning undefined pauses polling; a new key clears the old data and polls now. A failed poll
 * keeps the last data and sets `error` (show both: the data may be stale).
 */
export function usePolling<K, T>(
  key: () => K | undefined,
  fetch: (key: K, signal: AbortSignal) => Promise<T>,
  everyMs: number
) {
  const data = ref<T>()
  const error = ref('')
  const loading = ref(false)
  let timer: ReturnType<typeof setTimeout> | undefined
  let inflight: AbortController | undefined
  let again = false
  let active = false

  const visible = () => typeof document === 'undefined' || !document.hidden

  /** Abort the request in flight (its answer is dropped) and the pending timer. */
  function stop() {
    inflight?.abort()
    inflight = undefined
    again = false
    clearTimeout(timer)
    timer = undefined
    loading.value = false
  }

  async function tick() {
    if (inflight) {
      again = true
      return
    }
    clearTimeout(timer)
    timer = undefined
    const k = key()
    if (k === undefined || !active || !visible()) return
    const mine = new AbortController()
    inflight = mine
    loading.value = true
    try {
      const v = await fetch(k, mine.signal)
      if (!mine.signal.aborted) {
        data.value = v
        error.value = ''
      }
    } catch (e) {
      if (!mine.signal.aborted) error.value = e instanceof ApiError ? e.message : String(e)
    } finally {
      if (inflight === mine) {
        inflight = undefined
        loading.value = false
        if (again) {
          again = false
          void tick()
        } else if (active && visible()) {
          timer = setTimeout(tick, everyMs)
        }
      }
    }
  }

  const onVisibility = () => {
    stop()
    if (visible()) void tick()
  }
  watch(key, () => {
    stop()
    data.value = undefined
    error.value = ''
    void tick()
  })
  onMounted(() => {
    active = true
    document.addEventListener('visibilitychange', onVisibility)
    void tick()
  })
  onBeforeUnmount(() => {
    active = false
    stop()
    document.removeEventListener('visibilitychange', onVisibility)
  })
  return { data, error, loading, refresh: tick }
}
