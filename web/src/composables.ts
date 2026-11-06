import { useQuery, useQueryCache } from '@pinia/colada'
import { onBeforeUnmount, onMounted } from 'vue'
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
      cache.invalidateQueries({ key: ['settings'] })
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
 * Keep queries fresh from the server's event stream (`/api/v1/events`). After a reconnect, or when
 * the server says this client missed events, everything is refetched.
 */
export function useLiveUpdates() {
  const cache = useQueryCache()
  const refetch = (...keys: string[]) =>
    Promise.allSettled(keys.map((k) => cache.invalidateQueries({ key: [k] })))
  const everything = () => refetch('channels', 'playlists', 'playlist', 'epg-sources', 'hooks', 'settings', 'status')
  let es: EventSource | undefined
  onMounted(() => {
    if (typeof EventSource === 'undefined') return
    es = new EventSource('/api/v1/events')
    let connected = false
    es.addEventListener('ready', () => {
      if (connected) everything()
      connected = true
    })
    es.addEventListener('resync', everything)
    es.addEventListener('playlist.changed', () => refetch('channels', 'playlists', 'playlist'))
    es.addEventListener('epg.updated', () => refetch('epg-sources'))
  })
  onBeforeUnmount(() => es?.close())
}
