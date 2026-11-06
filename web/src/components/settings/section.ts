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

const rev = (s: Settings) => s.revision ?? 0

export interface SectionOptions<V> {
  /** A message when the draft cannot be saved; saving is disabled while there is one. */
  validate?: (draft: V) => string | null
}

/**
 * One settings card: the last values confirmed by the server for its keys, the draft being edited,
 * and save state.
 *
 * Copies of the settings arrive from refetches and from save responses in any order; the server's
 * `revision` says which is newer, and older copies are ignored. A newer copy updates a clean card;
 * a card with edits (or a save in flight) keeps the draft and flags a conflict only when one of its
 * own keys changed to something other than the draft.
 */
export function useSettingsSection<K extends keyof Settings>(
  keys: K[],
  settings: Ref<Settings | undefined>,
  options: SectionOptions<Pick<Settings, K>> = {}
) {
  type Values = Pick<Settings, K>
  const pick = (s: Settings) => Object.fromEntries(keys.map((k) => [k, s[k]])) as Values
  const same = (a: Values, b: Values) => keys.every((k) => norm(a[k]) === norm(b[k]))

  const confirmed = ref<Values | null>(null) as Ref<Values | null>
  const draft = ref({} as Values) as Ref<Values>
  const pending = ref(false)
  const error = ref('')
  const conflict = ref(false)
  /** Revision of the newest settings copy applied. */
  let revision = -1

  const dirty = computed(() => !!confirmed.value && !same(draft.value, confirmed.value))
  const invalid = computed(() => (options.validate ? options.validate(draft.value) : null))

  /** Apply a copy of the settings if it is newer than what this card has. */
  function apply(s: Settings) {
    if (rev(s) <= revision) return
    revision = rev(s)
    const next = pick(s)
    const prev = confirmed.value
    const editing = dirty.value || pending.value // before confirmed moves
    confirmed.value = next
    if (!prev || !editing) {
      draft.value = { ...next }
      return
    }
    // Editing: keep the draft; flag only a real change to this card's keys that the draft lacks.
    if (!same(next, prev) && !same(next, draft.value)) conflict.value = true
  }

  watch(settings, (s) => s && apply(s), { immediate: true })

  function reset() {
    if (confirmed.value) draft.value = { ...confirmed.value }
    conflict.value = false
    error.value = ''
  }

  /** Save this card's keys only. Returns whether it worked; errors stay on the card. */
  async function save(): Promise<boolean> {
    if (!dirty.value || pending.value || invalid.value) return false
    pending.value = true
    error.value = ''
    const sent = { ...draft.value }
    try {
      const response = await api.patchSettings(Object.fromEntries(keys.map((k) => [k, sent[k]])))
      if (rev(response) > revision) {
        // Nothing newer seen meanwhile: the response is the current state.
        revision = rev(response)
        confirmed.value = pick(response)
        draft.value = pick(response)
        conflict.value = false
      } else {
        // A newer copy (applied while saving) wins; say so if it overrode what was just saved.
        draft.value = { ...confirmed.value! }
        conflict.value = !same(confirmed.value!, pick(response))
      }
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
