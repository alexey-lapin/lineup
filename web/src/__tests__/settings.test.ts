import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { nextTick, ref } from 'vue'
import { deferred, flush } from './setup'

const api = vi.hoisted(() => ({ patchSettings: vi.fn(), testSetting: vi.fn() }))
vi.mock('../api', async (orig) => ({ ...(await orig<typeof import('../api')>()), api }))

import { ApiError, type Settings } from '../api'
import { useSettingsSection } from '../components/settings/section'
import BaseUrlField from '../components/settings/BaseUrlField.vue'

const base: Settings = {
  acexy_base: 'http://a:8080',
  engine_base: 'http://e:6878',
  vk_base: 'http://v:8099',
  epg_refresh_hours: 6,
  probe_interval_hours: 0
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
    api.patchSettings.mockResolvedValueOnce({ ...base, probe_interval_hours: 6 })
    expect(await engine.save()).toBe(true)
    expect(api.patchSettings).toHaveBeenCalledWith({ engine_base: 'http://e:6878', probe_interval_hours: 6 })
    expect(engine.dirty).toBe(false)
  })

  it('keeps a draft when another card changes, flags a conflict only for its own keys', async () => {
    const settings = ref<Settings | undefined>({ ...base })
    const vk = useSettingsSection(['vk_base'], settings)
    const guide = useSettingsSection(['epg_refresh_hours'], settings)
    vk.draft.vk_base = 'http://mine:8099'

    settings.value = { ...base, epg_refresh_hours: 12 }
    await nextTick()
    expect(vk.draft.vk_base).toBe('http://mine:8099')
    expect(vk.conflict).toBe(false)
    expect(guide.draft.epg_refresh_hours).toBe(12) // clean cards follow the server

    settings.value = { ...base, epg_refresh_hours: 12, vk_base: 'http://other:8099' }
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
    settings.value = { ...base, acexy_base: 'http://new:8080' } // settings.changed refetch
    await nextTick()
    pending.resolve({ ...base, acexy_base: 'http://new:8080' })
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
