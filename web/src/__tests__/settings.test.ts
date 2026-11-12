import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { nextTick, ref } from 'vue'
import { deferred, flush } from './setup'

const api = vi.hoisted(() => ({ patchSettings: vi.fn(), testIntegration: vi.fn() }))
vi.mock('../api', async (orig) => ({ ...(await orig<typeof import('../api')>()), api }))

import { ApiError, type Settings } from '../api'
import { baseUrlError, useSection, useSettingsSection } from '../components/settings/section'
import SettingsCard from '../components/settings/SettingsCard.vue'

/** A server object edited by two cards (as the Acestream card's fields and another card would be). */
type Thing = { engine: string; acexy: string; hours: number; revision: number }
const base: Thing = { engine: 'http://e:6878', acexy: 'http://a:8080', hours: 6, revision: 1 }
const at = (revision: number, changes: Partial<Thing>): Thing => ({ ...base, ...changes, revision })

const patch = vi.fn<(d: Record<string, string | number>) => Promise<Thing>>()
const card = <K extends keyof Omit<Thing, 'revision'>>(
  source: ReturnType<typeof ref<Thing | undefined>>,
  keys: K[],
  validate?: (d: Pick<Thing, K>) => string | null
) =>
  useSection<Thing, Pick<Thing, K>>(
    source,
    (s) => Object.fromEntries(keys.map((k) => [k, s[k]])) as Pick<Thing, K>,
    (d) => patch(d),
    { validate }
  )

beforeEach(() => {
  api.patchSettings.mockReset()
  api.testIntegration.mockReset()
  patch.mockReset()
})

describe('useSection', () => {
  it('saves its draft through its patch', async () => {
    const source = ref<Thing | undefined>({ ...base })
    const c = card(source, ['engine', 'hours'])
    c.draft.hours = 12
    patch.mockResolvedValueOnce(at(2, { hours: 12 }))
    expect(await c.save()).toBe(true)
    expect(patch).toHaveBeenCalledWith({ engine: 'http://e:6878', hours: 12 })
    expect(c.dirty).toBe(false)
  })

  it('keeps a draft when another card changes, flags a conflict only for its own values', async () => {
    const source = ref<Thing | undefined>({ ...base })
    const acexy = card(source, ['acexy'])
    const hours = card(source, ['hours'])
    acexy.draft.acexy = 'http://mine:8080'

    source.value = at(2, { hours: 12 })
    await nextTick()
    expect(acexy.draft.acexy).toBe('http://mine:8080')
    expect(acexy.conflict).toBe(false)
    expect(hours.draft.hours).toBe(12) // clean cards follow the server

    source.value = at(3, { hours: 12, acexy: 'http://other:8080' })
    await nextTick()
    expect(acexy.draft.acexy).toBe('http://mine:8080')
    expect(acexy.conflict).toBe(true)
    acexy.reset()
    expect([acexy.draft.acexy, acexy.conflict, acexy.dirty]).toEqual(['http://other:8080', false, false])
  })

  it('treats its own save, echoed back before the response, as no conflict', async () => {
    const source = ref<Thing | undefined>({ ...base })
    const acexy = card(source, ['acexy'])
    acexy.draft.acexy = 'http://new:8080/'
    const pending = deferred<Thing>()
    patch.mockReturnValueOnce(pending.promise)
    const saving = acexy.save()
    source.value = at(2, { acexy: 'http://new:8080' }) // integration.changed refetch
    await nextTick()
    pending.resolve(at(2, { acexy: 'http://new:8080' }))
    expect(await saving).toBe(true)
    expect([acexy.conflict, acexy.dirty]).toEqual([false, false])
  })

  it('keeps the draft and shows the server error when a save is rejected', async () => {
    const source = ref<Thing | undefined>({ ...base })
    const c = card(source, ['acexy'])
    c.draft.acexy = 'ftp://x'
    patch.mockRejectedValueOnce(new ApiError('invalid', 'acexy_url: must be an http(s) URL with a host', 422))
    expect(await c.save()).toBe(false)
    expect(c.error).toContain('must be an http(s) URL')
    expect(c.draft.acexy).toBe('ftp://x')
    expect(c.dirty).toBe(true)
  })

  it('shows a newer value seen during the save instead of the older response, and says so', async () => {
    const source = ref<Thing | undefined>({ ...base })
    const c = card(source, ['acexy'])
    c.draft.acexy = 'http://mine:8080'
    const pending = deferred<Thing>()
    patch.mockReturnValueOnce(pending.promise)
    const saving = c.save()
    source.value = at(3, { acexy: 'http://newer:8080' }) // another client, after our write (2)
    await nextTick()
    pending.resolve(at(2, { acexy: 'http://mine:8080' }))
    expect(await saving).toBe(true)
    expect([c.draft.acexy, c.dirty, c.conflict]).toEqual(['http://newer:8080', false, true])
  })

  it('takes its own response when the copies seen meanwhile are older', async () => {
    const source = ref<Thing | undefined>({ ...base })
    const c = card(source, ['acexy'])
    c.draft.acexy = 'http://mine:8080'
    const pending = deferred<Thing>()
    patch.mockReturnValueOnce(pending.promise)
    const saving = c.save()
    source.value = at(2, { hours: 12 }) // another card changed before our write
    await nextTick()
    pending.resolve(at(3, { hours: 12, acexy: 'http://mine:8080' }))
    expect(await saving).toBe(true)
    expect([c.draft.acexy, c.dirty, c.conflict]).toEqual(['http://mine:8080', false, false])
    source.value = at(4, { hours: 24, acexy: 'http://mine:8080' })
    await nextTick()
    expect([c.draft.acexy, c.conflict]).toEqual(['http://mine:8080', false])
  })

  it('ignores copies older than what it has', async () => {
    const source = ref<Thing | undefined>(at(5, { acexy: 'http://now:8080' }))
    const c = card(source, ['acexy'])
    source.value = at(4, { acexy: 'http://old:8080' })
    await nextTick()
    expect(c.draft.acexy).toBe('http://now:8080')
  })

  it('does not save an invalid draft', async () => {
    const source = ref<Thing | undefined>({ ...base })
    const c = card(source, ['acexy'], (d) => baseUrlError(d.acexy, false))
    c.draft.acexy = 'http://x/?a=1'
    expect(c.invalid).toBe('Must not contain a query or fragment')
    expect(await c.save()).toBe(false)
    expect(patch).not.toHaveBeenCalled()
  })

  it('disables Reset while a save is pending', async () => {
    const section = { dirty: true, invalid: null, pending: true, conflict: false, error: '', reset: vi.fn(), save: vi.fn() }
    const UButton = { props: ['label', 'disabled'], template: '<button :disabled="disabled">{{ label }}</button>' }
    const w = mount(SettingsCard, {
      props: { title: 't', icon: 'i', section },
      global: { stubs: { UButton, UCard: { template: '<div><slot /></div>' } } }
    })
    const reset = w.findAll('button').find((b) => b.text() === 'Reset')
    expect(reset?.attributes('disabled')).toBeDefined()
  })
})

describe('useSettingsSection', () => {
  it('sends only its keys', async () => {
    const settings = ref<Settings | undefined>({ epg_refresh_hours: 6, check_interval_hours: 0, revision: 1 })
    const guide = useSettingsSection(['epg_refresh_hours'], settings)
    guide.draft.epg_refresh_hours = 12
    api.patchSettings.mockResolvedValueOnce({ epg_refresh_hours: 12, check_interval_hours: 0, revision: 2 })
    expect(await guide.save()).toBe(true)
    expect(api.patchSettings).toHaveBeenCalledWith({ epg_refresh_hours: 12 })
  })
})

describe('baseUrlError', () => {
  it('mirrors the server rules', () => {
    expect(baseUrlError('http://media-host:8080')).toBeNull()
    expect(baseUrlError('https://h/prefix/')).toBeNull()
    expect(baseUrlError('')).toBe('Required')
    expect(baseUrlError('', false)).toBeNull()
    expect(baseUrlError('ftp://h')).toMatch(/http/)
    expect(baseUrlError('http://h#x')).toMatch(/query or fragment/)
    expect(baseUrlError('nope')).toBe('Not a valid URL')
  })
})
