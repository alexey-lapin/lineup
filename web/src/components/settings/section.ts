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
export function baseUrlError(value: string, required = true): string | null {
  const v = value.trim()
  if (!v) return required ? 'Required' : null
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

/** Whole hours from 0 to 168, as the server accepts. */
export const hoursError = (v: number) =>
  Number.isInteger(v) && v >= 0 && v <= 168 ? null : 'Enter whole hours from 0 to 168'

/** A server object whose copies are ordered by `revision` (higher is newer). */
export interface Versioned {
  revision?: number
}


export interface SectionOptions<V> {
  /** A message when the draft cannot be saved; saving is disabled while there is one. */
  validate?: (draft: V) => string | null
}

/**
 * One editable card: the last values confirmed by the server (`pick` of the source), the draft
 * being edited, and save state.
 *
 * Copies of the source arrive from refetches and from save responses in any order; the server's
 * `revision` says which is newer, and older copies are ignored. A newer copy updates a clean card;
 * a card with edits (or a save in flight) keeps the draft and flags a conflict only when one of its
 * own values changed to something other than the draft.
 */
export function useSection<T extends Versioned, V extends object>(
  source: Ref<T | undefined>,
  pick: (s: T) => V,
  patch: (draft: V) => Promise<T>,
  options: SectionOptions<V> = {}
) {
  const rev = (s: T) => s.revision ?? 0
  const same = (a: V, b: V) =>
    Object.keys(a).every((k) => norm((a as Record<string, unknown>)[k]) === norm((b as Record<string, unknown>)[k]))

  const confirmed = ref<V | null>(null) as Ref<V | null>
  const draft = ref({} as V) as Ref<V>
  const pending = ref(false)
  const error = ref('')
  const conflict = ref(false)
  /** Revision of the newest copy applied. */
  let revision = -1

  const dirty = computed(() => !!confirmed.value && !same(draft.value, confirmed.value))
  const invalid = computed(() => (options.validate ? options.validate(draft.value) : null))

  /** Apply a copy if it is newer than what this card has. */
  function apply(s: T) {
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
    // Editing: keep the draft; flag only a real change to this card's values that the draft lacks.
    if (!same(next, prev) && !same(next, draft.value)) conflict.value = true
  }

  watch(source, (s) => s && apply(s), { immediate: true })

  function reset() {
    if (confirmed.value) draft.value = { ...confirmed.value }
    conflict.value = false
    error.value = ''
  }

  /** Save the draft. Returns whether it worked; errors stay on the card. */
  async function save(): Promise<boolean> {
    if (!dirty.value || pending.value || invalid.value) return false
    pending.value = true
    error.value = ''
    const sent = { ...draft.value }
    try {
      const response = await patch(sent)
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

  return reactive({ confirmed, draft, dirty, invalid, pending, error, conflict, reset, save })
}

/** A card over some settings keys; saving sends only those keys. */
export function useSettingsSection<K extends keyof Settings & string>(
  keys: K[],
  settings: Ref<Settings | undefined>,
  options: SectionOptions<Record<K, Settings[K]>> = {}
) {
  type V = Record<K, Settings[K]>
  const pick = (s: Settings) => Object.fromEntries(keys.map((k) => [k, s[k]])) as V
  return useSection<Settings, V>(settings, pick, (d) => api.patchSettings(pick(d as unknown as Settings)), options)
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
