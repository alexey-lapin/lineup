import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { deferred, flush, plugins } from './setup'

const api = vi.hoisted(() => ({
  channels: vi.fn(),
  playlists: vi.fn(),
  playlist: vi.fn(),
  resolve: vi.fn(),
  createChannel: vi.fn(),
  replaceItems: vi.fn(),
  deletePlaylist: vi.fn(),
  deleteChannel: vi.fn(),
  importM3u: vi.fn(),
  status: vi.fn(),
  settings: vi.fn()
}))
vi.mock('../api', async (orig) => ({ ...(await orig<typeof import('../api')>()), api }))

import { useAction } from '../composables'
import ChannelForm from '../components/ChannelForm.vue'
import PlaylistPage from '../pages/PlaylistPage.vue'
import ImportPage from '../pages/ImportPage.vue'

const ACE_A = 'a'.repeat(40)
const ch = (id: number, name: string) => ({
  id,
  name,
  enabled: true,
  is_event: false,
  playlists: [],
  source: { kind: 'acestream', id: ACE_A },
  source_key: ACE_A,
  stream_url: 'http://x',
  created_at: '',
  updated_at: ''
})
const detail = (slug: string, ids: number[]) => ({
  id: 1,
  slug,
  name: slug,
  channel_count: ids.length,
  created_at: '',
  updated_at: '',
  items: ids.map((id, i) => ({ position: i + 1, channel: ch(id, `c${id}`) }))
})

beforeEach(() => {
  for (const f of Object.values(api)) f.mockReset()
  api.channels.mockResolvedValue([])
  api.playlists.mockResolvedValue([])
})

describe('useAction', () => {
  it('reports success for 204 responses (undefined value)', async () => {
    const { list } = plugins()
    let run!: ReturnType<typeof useAction>
    mount({ template: '<div/>', setup: () => void (run = useAction()) }, { global: { plugins: list } })
    expect(await run(async () => undefined)).toEqual({ ok: true, value: undefined })
    expect(await run(async () => Promise.reject(new Error('x')))).toEqual({ ok: false })
  })
})

describe('ChannelForm', () => {
  it('drops a resolution that arrives after the form was closed and reopened', async () => {
    const { list } = plugins()
    const pending = deferred<unknown>()
    api.resolve.mockReturnValueOnce(pending.promise)
    const w = mount(ChannelForm, { props: { open: true }, shallow: true, global: { plugins: list } })
    const vm = w.vm as any
    vm.state.input = `acestream://${ACE_A}`
    vm.resolveInput()
    await w.setProps({ open: false })
    await w.setProps({ open: true })
    pending.resolve({ source: { kind: 'acestream', id: ACE_A }, source_key: ACE_A, stream_url: 'u', duplicate_of: null })
    await flush()
    expect(vm.currentResolution).toBeNull()
    expect(vm.canSave).toBe(false)
    vm.state.name = 'X'
    await vm.save()
    expect(api.createChannel).not.toHaveBeenCalled()
  })

  it('invalidates a resolution when the input changes', async () => {
    const { list } = plugins()
    api.resolve.mockResolvedValue({ source: { kind: 'acestream', id: ACE_A }, source_key: ACE_A, stream_url: 'u', duplicate_of: null })
    const w = mount(ChannelForm, { props: { open: true }, shallow: true, global: { plugins: list } })
    const vm = w.vm as any
    vm.state.input = ACE_A
    await vm.resolveInput()
    vm.state.name = 'X'
    expect(vm.canSave).toBe(true)
    vm.state.input = 'b'.repeat(40)
    await flush()
    expect(vm.canSave).toBe(false)
  })
})

describe('PlaylistPage', () => {
  it('ignores a save response for a playlist the user navigated away from', async () => {
    const { list } = plugins()
    api.playlist.mockImplementation(async (slug: string) => detail(slug, slug === 'a' ? [1, 2] : [3]))
    const pending = deferred<unknown>()
    api.replaceItems.mockReturnValueOnce(pending.promise)
    const w = mount(PlaylistPage, { props: { slug: 'a' }, shallow: true, global: { plugins: list } })
    await flush()
    const vm = w.vm as any
    vm.save([2, 1])
    await w.setProps({ slug: 'b' })
    await flush()
    pending.resolve(detail('a', [2, 1]))
    await flush()
    expect(vm.items.map((i: any) => i.channel.id)).toEqual([3])
  })

  it('rolls back to the confirmed order when a save fails, and blocks overlapping saves', async () => {
    const { list } = plugins()
    api.playlist.mockResolvedValue(detail('a', [1, 2]))
    const pending = deferred<unknown>()
    api.replaceItems.mockReturnValueOnce(pending.promise)
    const w = mount(PlaylistPage, { props: { slug: 'a' }, shallow: true, global: { plugins: list } })
    await flush()
    const vm = w.vm as any
    vm.items = [vm.items[1], vm.items[0]] // optimistic drag
    vm.save([2, 1])
    await vm.save([1]) // overlapping save is ignored while one is in flight
    expect(api.replaceItems).toHaveBeenCalledTimes(1)
    api.playlist.mockRejectedValue(new Error('refetch failed'))
    pending.reject(new Error('PUT failed'))
    await flush()
    expect(vm.items.map((i: any) => i.channel.id)).toEqual([1, 2])
  })
})

describe('PlaylistPage cached route', () => {
  it('rolls back correctly after returning to a cached playlist', async () => {
    const { list } = plugins()
    api.playlist.mockImplementation(async (slug: string) => detail(slug, slug === 'a' ? [1, 2] : [3]))
    const w = mount(PlaylistPage, { props: { slug: 'a' }, shallow: true, global: { plugins: list } })
    await flush()
    await w.setProps({ slug: 'b' })
    await flush()
    await w.setProps({ slug: 'a' }) // served from cache (fresh)
    await flush()
    const vm = w.vm as any
    expect(vm.items.map((i: any) => i.channel.id)).toEqual([1, 2])
    api.replaceItems.mockRejectedValueOnce(new Error('PUT failed'))
    api.playlist.mockRejectedValue(new Error('refetch failed'))
    vm.items = [vm.items[1], vm.items[0]]
    await vm.save([2, 1])
    expect(vm.items.map((i: any) => i.channel.id)).toEqual([1, 2])
  })
})

describe('ImportPage', () => {
  it('commits exactly the previewed snapshot and drops stale previews', async () => {
    const { list } = plugins()
    const pending = deferred<unknown>()
    api.importM3u.mockReturnValueOnce(pending.promise)
    const w = mount(ImportPage, { shallow: true, global: { plugins: list } })
    const vm = w.vm as any
    vm.text = 'TEXT-A'
    vm.slug = 'a'
    await flush()
    vm.doPreview()
    vm.text = 'TEXT-B' // edit while the preview is in flight
    vm.slug = 'b'
    await flush()
    pending.resolve({ dry_run: true, playlist_slug: 'a', playlist_created: true, entries: [{}], skipped_lines: [] })
    await flush()
    expect(vm.preview).toBeNull()
    expect(vm.previewIsCurrent).toBe(false)
    await vm.commit()
    expect(api.importM3u).toHaveBeenCalledTimes(1)

    api.importM3u.mockResolvedValueOnce({ dry_run: true, playlist_slug: 'b', playlist_created: true, entries: [{}], skipped_lines: [] })
    await vm.doPreview()
    expect(vm.previewIsCurrent).toBe(true)
    api.importM3u.mockResolvedValueOnce({ dry_run: false, playlist_slug: 'b', playlist_created: true, entries: [{}], skipped_lines: [] })
    await vm.commit()
    expect(api.importM3u).toHaveBeenLastCalledWith('TEXT-B', 'b', false)
  })
})

describe('ChannelForm suggestions', () => {
  const resolution = (input: string, suggestion: Record<string, unknown>) => ({
    source: { kind: 'direct', url: input },
    source_key: input,
    stream_url: input,
    configured: true,
    duplicate_of: null,
    matches: [],
    warnings: [],
    suggestion: { group_title: null, tvg_id: null, tvg_logo: null, is_event: false, ...suggestion }
  })

  it('replaces values it filled in, clears them when the next source suggests nothing, keeps edits', async () => {
    const { list } = plugins()
    const w = mount(ChannelForm, { props: { open: true }, shallow: true, global: { plugins: list } })
    const vm = w.vm as any

    api.resolve.mockResolvedValueOnce(
      resolution('http://a', { name: 'VK 1_2', group_title: 'VK', tvg_id: 'A.uk', tvg_logo: 'http://l', is_event: true })
    )
    vm.state.input = 'http://a'
    await vm.resolveInput()
    expect([vm.state.group_title, vm.state.tvg_id, vm.state.is_event]).toEqual(['VK', 'A.uk', true])

    // The user edits the logo; the next source suggests nothing beyond a name.
    vm.state.tvg_logo = 'http://mine'
    api.resolve.mockResolvedValueOnce(resolution('http://b', { name: 'b' }))
    vm.state.input = 'http://b'
    await flush()
    await vm.resolveInput()
    expect(vm.state.name).toBe('b')
    expect(vm.state.group_title).toBe('')
    expect(vm.state.tvg_id).toBe('')
    expect(vm.state.is_event).toBe(false)
    expect(vm.state.tvg_logo).toBe('http://mine')

    // An event flag the user set by hand stays.
    vm.state.is_event = true
    api.resolve.mockResolvedValueOnce(resolution('http://c', { name: 'c' }))
    vm.state.input = 'http://c'
    await flush()
    await vm.resolveInput()
    expect(vm.state.is_event).toBe(true)
  })
})
