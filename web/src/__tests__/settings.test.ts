import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { nextTick, ref } from 'vue'
import { deferred, flush } from './setup'

const api = vi.hoisted(() => ({ patchSettings: vi.fn(), testSetting: vi.fn() }))
vi.mock('../api', async (orig) => ({ ...(await orig<typeof import('../api')>()), api }))

import { ApiError, type Settings } from '../api'
import { baseUrlError, useSettingsSection } from '../components/settings/section'
import SettingsCard from '../components/settings/SettingsCard.vue'
import BaseUrlField from '../components/settings/BaseUrlField.vue'

const base: Settings = {
  acexy_base: 'http://a:8080',
  engine_base: 'http://e:6878',
  vk_base: 'http://v:8099',
  epg_refresh_hours: 6,
  probe_interval_hours: 0,
  revision: 1
}

beforeEach(() => {
  api.patchSettings.mockReset()
  api.testSetting.mockReset()
})

describe('useSettingsSection', () => {
  it('saves only its own keys', async () => {
    const settings = ref<Settings | undefined>({ ...base })
    const engine = useSettingsSection(['engine_base', 'probe_interval_hours'], settings)
    engine.draft.probe_interval_hours = 6
    api.patchSettings.mockResolvedValueOnce({ ...base, probe_interval_hours: 6, revision: 2 })
    expect(await engine.save()).toBe(true)
    expect(api.patchSettings).toHaveBeenCalledWith({ engine_base: 'http://e:6878', probe_interval_hours: 6 })
    expect(engine.dirty).toBe(false)
  })

  it('keeps a draft when another card changes, flags a conflict only for its own keys', async () => {
    const settings = ref<Settings | undefined>({ ...base })
    const vk = useSettingsSection(['vk_base'], settings)
    const guide = useSettingsSection(['epg_refresh_hours'], settings)
    vk.draft.vk_base = 'http://mine:8099'

    settings.value = { ...base, epg_refresh_hours: 12, revision: 2 }
    await nextTick()
    expect(vk.draft.vk_base).toBe('http://mine:8099')
    expect(vk.conflict).toBe(false)
    expect(guide.draft.epg_refresh_hours).toBe(12) // clean cards follow the server

    settings.value = { ...base, epg_refresh_hours: 12, vk_base: 'http://other:8099', revision: 3 }
    await nextTick()
    expect(vk.draft.vk_base).toBe('http://mine:8099')
    expect(vk.conflict).toBe(true)
    vk.reset()
    expect([vk.draft.vk_base, vk.conflict, vk.dirty]).toEqual(['http://other:8099', false, false])
  })

  it('treats its own save, echoed back before the response, as no conflict', async () => {
    const settings = ref<Settings | undefined>({ ...base })
    const acexy = useSettingsSection(['acexy_base'], settings)
    acexy.draft.acexy_base = 'http://new:8080/'
    const pending = deferred<Settings>()
    api.patchSettings.mockReturnValueOnce(pending.promise)
    const saving = acexy.save()
    settings.value = { ...base, acexy_base: 'http://new:8080', revision: 2 } // settings.changed refetch
    await nextTick()
    pending.resolve({ ...base, acexy_base: 'http://new:8080', revision: 2 })
    expect(await saving).toBe(true)
    expect([acexy.conflict, acexy.dirty]).toEqual([false, false])
  })

  it('keeps the draft and shows the server error when a save is rejected', async () => {
    const settings = ref<Settings | undefined>({ ...base })
    const vk = useSettingsSection(['vk_base'], settings)
    vk.draft.vk_base = 'ftp://x'
    api.patchSettings.mockRejectedValueOnce(new ApiError('invalid', 'vk_base: must be an http(s) URL with a host', 422))
    expect(await vk.save()).toBe(false)
    expect(vk.error).toContain('must be an http(s) URL')
    expect(vk.draft.vk_base).toBe('ftp://x')
    expect(vk.dirty).toBe(true)
  })
})

describe('saving with newer server changes', () => {
  // Copies of the settings carry the server's revision; the higher one is newer, whatever the
  // order in which they arrive.
  const at = (revision: number, changes: Partial<Settings>) => ({ ...base, ...changes, revision })

  it('shows a newer value seen during the save instead of the older response, and says so', async () => {
    const settings = ref<Settings | undefined>({ ...base })
    const vk = useSettingsSection(['vk_base'], settings)
    vk.draft.vk_base = 'http://mine:8099'
    const pending = deferred<Settings>()
    api.patchSettings.mockReturnValueOnce(pending.promise)
    const saving = vk.save()
    settings.value = at(3, { vk_base: 'http://newer:8099' }) // another client, after our write (2)
    await nextTick()
    pending.resolve(at(2, { vk_base: 'http://mine:8099' }))
    expect(await saving).toBe(true)
    expect([vk.draft.vk_base, vk.dirty, vk.conflict]).toEqual(['http://newer:8099', false, true])
  })

  it('takes its own response when the copies seen meanwhile are older', async () => {
    const settings = ref<Settings | undefined>({ ...base })
    const vk = useSettingsSection(['vk_base'], settings)
    vk.draft.vk_base = 'http://mine:8099'
    const pending = deferred<Settings>()
    api.patchSettings.mockReturnValueOnce(pending.promise)
    const saving = vk.save()
    settings.value = at(2, { epg_refresh_hours: 12 }) // another card changed before our write
    await nextTick()
    pending.resolve(at(3, { epg_refresh_hours: 12, vk_base: 'http://mine:8099' }))
    expect(await saving).toBe(true)
    expect([vk.draft.vk_base, vk.dirty, vk.conflict]).toEqual(['http://mine:8099', false, false])
    // A later change to another card only (it carries our value) raises nothing.
    settings.value = at(4, { epg_refresh_hours: 24, vk_base: 'http://mine:8099' })
    await nextTick()
    expect([vk.draft.vk_base, vk.conflict]).toEqual(['http://mine:8099', false])
  })

  it('treats the echo of its own save as no conflict, in either order', async () => {
    const settings = ref<Settings | undefined>({ ...base })
    const vk = useSettingsSection(['vk_base'], settings)
    vk.draft.vk_base = 'http://mine:8099'
    const pending = deferred<Settings>()
    api.patchSettings.mockReturnValueOnce(pending.promise)
    const saving = vk.save()
    settings.value = at(2, { vk_base: 'http://mine:8099' })
    await nextTick()
    pending.resolve(at(2, { vk_base: 'http://mine:8099' }))
    expect(await saving).toBe(true)
    expect([vk.dirty, vk.conflict]).toEqual([false, false])
  })

  it('ignores copies older than what it has', async () => {
    const settings = ref<Settings | undefined>(at(5, { vk_base: 'http://now:8099' }))
    const vk = useSettingsSection(['vk_base'], settings)
    settings.value = at(4, { vk_base: 'http://old:8099' })
    await nextTick()
    expect(vk.draft.vk_base).toBe('http://now:8099')
  })

  it('does not save an invalid draft', async () => {
    const settings = ref<Settings | undefined>({ ...base })
    const vk = useSettingsSection(['vk_base'], settings, { validate: (d) => baseUrlError(d.vk_base) })
    vk.draft.vk_base = 'http://x/?a=1'
    expect(vk.invalid).toBe('Must not contain a query or fragment')
    expect(await vk.save()).toBe(false)
    expect(api.patchSettings).not.toHaveBeenCalled()
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

describe('baseUrlError', () => {
  it('mirrors the server rules', () => {
    expect(baseUrlError('http://media-host:8080')).toBeNull()
    expect(baseUrlError('https://h/prefix/')).toBeNull()
    expect(baseUrlError('')).toBe('Required')
    expect(baseUrlError('ftp://h')).toMatch(/http/)
    expect(baseUrlError('http://h#x')).toMatch(/query or fragment/)
    expect(baseUrlError('nope')).toBe('Not a valid URL')
  })
})

describe('BaseUrlField', () => {
  it('drops a test result when the URL changed while it was running', async () => {
    const pending = deferred<unknown>()
    api.testSetting.mockReturnValueOnce(pending.promise)
    const w = mount(BaseUrlField, {
      props: { modelValue: 'http://a:8080', label: 'URL', help: '', target: 'acexy', placeholder: '' },
      shallow: true
    })
    const vm = w.vm as any
    vm.test()
    expect(api.testSetting).toHaveBeenCalledWith('acexy', 'http://a:8080')
    await w.setProps({ modelValue: 'http://b:8080' })
    pending.resolve({ ok: true, url: 'http://a:8080/ace/status', detail: 'acexy answered', elapsed_ms: 3 })
    await flush()
    expect(vm.result).toBeNull()
    expect(vm.testing).toBe(false)
  })
})
