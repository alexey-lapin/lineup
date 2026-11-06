import { computed, reactive, ref, watch, type Ref } from 'vue'
import { api, ApiError, type Settings } from '../../api'

/** Values compared for "unsaved changes": trimmed, trailing slashes dropped, numbers as numbers. */
function norm(v: unknown): string {
  if (typeof v === 'number') return String(v)
  return String(v ?? '')
    .trim()
    .replace(/\/+$/, '')
}

/**
 * One settings card: the last values confirmed by the server for its keys, the draft being edited,
 * and save state. A refetch updates a clean card; a card with edits keeps them and flags a
 * conflict only when one of its own keys changed to something other than the draft.
 */
export function useSettingsSection<K extends keyof Settings>(keys: K[], settings: Ref<Settings | undefined>) {
  type Values = Pick<Settings, K>
  const pick = (s: Settings) => Object.fromEntries(keys.map((k) => [k, s[k]])) as Values

  const confirmed = ref<Values | null>(null) as Ref<Values | null>
  const draft = ref({} as Values) as Ref<Values>
  const pending = ref(false)
  const error = ref('')
  const conflict = ref(false)

  const dirty = computed(() => !!confirmed.value && keys.some((k) => norm(draft.value[k]) !== norm(confirmed.value![k])))

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

  /** Save this card's keys only. Returns whether it worked; errors stay on the card. */
  async function save(): Promise<boolean> {
    if (!dirty.value || pending.value) return false
    pending.value = true
    error.value = ''
    try {
      const body = Object.fromEntries(keys.map((k) => [k, draft.value[k]]))
      const saved = pick(await api.patchSettings(body))
      confirmed.value = saved
      draft.value = { ...saved }
      conflict.value = false
      return true
    } catch (e) {
      error.value = e instanceof ApiError ? e.message : String(e)
      return false
    } finally {
      pending.value = false
    }
  }

  return reactive({ keys, confirmed, draft, dirty, pending, error, conflict, reset, save })
}

/** What a settings card needs from its section. */
export interface SettingsSection {
  dirty: boolean
  pending: boolean
  conflict: boolean
  error: string
  reset(): void
  save(): Promise<boolean>
}
