import { computed, reactive, ref, watch, type Ref } from 'vue'
import { api, ApiError, type Settings } from '../../api'

/** Values compared for "unsaved changes": trimmed, trailing slashes dropped, numbers as numbers. */
function norm(v: unknown): string {
  if (typeof v === 'number') return String(v)
  return String(v ?? '')
    .trim()
    .replace(/\/+$/, '')
}

/** Mirror of the server's base URL rules, for messages before saving; the server stays authoritative. */
export function baseUrlError(value: string): string | null {
  const v = value.trim()
  if (!v) return 'Required'
  let u: URL
  try {
    u = new URL(v)
  } catch {
    return 'Not a valid URL'
  }
  if (u.protocol !== 'http:' && u.protocol !== 'https:') return 'Must start with http:// or https://'
  if (!u.hostname) return 'Needs a host'
  if (u.search || u.hash || v.includes('?') || v.includes('#')) return 'Must not contain a query or fragment'
  return null
}

export interface SectionOptions<V> {
  /** A message when the draft cannot be saved; saving is disabled while there is one. */
  validate?: (draft: V) => string | null
  /** Read settings from the server again (used after a save to show the current values). */
  reload?: () => Promise<Settings | undefined>
}

/**
 * One settings card: the last values confirmed by the server for its keys, the draft being edited,
 * and save state. A refetch updates a clean card; a card with edits keeps them and flags a
 * conflict only when one of its own keys changed to something other than the draft.
 */
export function useSettingsSection<K extends keyof Settings>(
  keys: K[],
  settings: Ref<Settings | undefined>,
  options: SectionOptions<Pick<Settings, K>> = {}
) {
  type Values = Pick<Settings, K>
  const pick = (s: Settings) => Object.fromEntries(keys.map((k) => [k, s[k]])) as Values

  const confirmed = ref<Values | null>(null) as Ref<Values | null>
  const draft = ref({} as Values) as Ref<Values>
  const pending = ref(false)
  const error = ref('')
  const conflict = ref(false)

  const dirty = computed(() => !!confirmed.value && keys.some((k) => norm(draft.value[k]) !== norm(confirmed.value![k])))
  const invalid = computed(() => (options.validate ? options.validate(draft.value) : null))

  watch(
    settings,
    (s) => {
      if (!s) return
      const next = pick(s)
      const prev = confirmed.value
      if (!prev) {
        confirmed.value = next
        draft.value = { ...next }
        return
      }
      if (!keys.some((k) => norm(next[k]) !== norm(prev[k]))) return
      if (dirty.value || pending.value) {
        if (keys.some((k) => norm(next[k]) !== norm(prev[k]) && norm(next[k]) !== norm(draft.value[k]))) {
          conflict.value = true
        }
        confirmed.value = next
      } else {
        confirmed.value = next
        draft.value = { ...next }
      }
    },
    { immediate: true }
  )

  function reset() {
    if (confirmed.value) draft.value = { ...confirmed.value }
    conflict.value = false
    error.value = ''
  }

  /**
   * Save this card's keys only. Returns whether it worked; errors stay on the card. The PATCH
   * response can be older than changes already seen from other clients, so afterwards the card
   * shows a fresh read of the server and flags it when someone else changed these keys since.
   */
  async function save(): Promise<boolean> {
    if (!dirty.value || pending.value || invalid.value) return false
    pending.value = true
    error.value = ''
    try {
      const body = Object.fromEntries(keys.map((k) => [k, draft.value[k]]))
      const saved = pick(await api.patchSettings(body))
      const fresh = options.reload ? await options.reload().catch(() => undefined) : undefined
      const current = fresh ? pick(fresh) : saved
      confirmed.value = current
      draft.value = { ...current }
      conflict.value = keys.some((k) => norm(current[k]) !== norm(saved[k]))
      return true
    } catch (e) {
      error.value = e instanceof ApiError ? e.message : String(e)
      return false
    } finally {
      pending.value = false
    }
  }

  return reactive({ keys, confirmed, draft, dirty, invalid, pending, error, conflict, reset, save })
}

/** What a settings card needs from its section. */
export interface SettingsSection {
  dirty: boolean
  invalid: string | null
  pending: boolean
  conflict: boolean
  error: string
  reset(): void
  save(): Promise<boolean>
}
