import createClient from 'openapi-fetch'
import type { components, paths } from './schema'

export type Channel = components['schemas']['Channel']
export type ChannelCreate = components['schemas']['ChannelCreate']
export type ChannelPatch = components['schemas']['ChannelPatch']
export type Source = components['schemas']['Source']
export type Playlist = components['schemas']['Playlist']
export type PlaylistDetail = components['schemas']['PlaylistDetail']
export type Settings = components['schemas']['Settings']
export type SettingsPatch = components['schemas']['SettingsPatch']
export type ImportReport = components['schemas']['ImportReport']
export type ResolveResponse = components['schemas']['ResolveResponse']
export type Status = components['schemas']['Status']
export type SourceKind = components['schemas']['SourceKind']
export type Integration = components['schemas']['Integration']
export type IntegrationConfig = components['schemas']['IntegrationConfig']
export type IntegrationPatch = components['schemas']['IntegrationPatch']
export type IntegrationStatus = components['schemas']['IntegrationStatus']
export type Component = components['schemas']['Component']
export type ConnectionTestResult = components['schemas']['ConnectionTestResult']
export type ChannelInfo = components['schemas']['ChannelInfo']
export type AcestreamLive = components['schemas']['AcestreamLive']
export type Suggestion = components['schemas']['Suggestion']
export type EpgSource = components['schemas']['EpgSource']
export type EpgSourceCreate = components['schemas']['EpgSourceCreate']
export type EpgSourcePatch = components['schemas']['EpgSourcePatch']
export type EpgChannel = components['schemas']['EpgChannel']
export type EpgFetchResult = components['schemas']['EpgFetchResult']
export type Hook = components['schemas']['Hook']
export type HookConfig = components['schemas']['HookConfig']
export type HookCreate = components['schemas']['HookCreate']
export type HookPatch = components['schemas']['HookPatch']
export type HookDelivery = components['schemas']['HookDelivery']

/** Event types hooks can subscribe to. */
export const EVENT_TYPES = ['playlist.changed', 'epg.updated', 'channel.unhealthy', 'channel.recovered'] as const
/** What the API shows instead of a stored secret; sending it back keeps the secret. */
export const SECRET_MASK = '********'

export const client = createClient<paths>({ baseUrl: '/api/v1' })

export class ApiError extends Error {
  constructor(
    public code: string,
    message: string,
    public status: number
  ) {
    super(message)
  }
}

type Result<T> = { data?: T; error?: unknown; response: Response }

/** Unwrap an openapi-fetch result: throw ApiError on failure. */
export async function unwrap<T>(p: Promise<Result<T>>): Promise<T> {
  let r: Result<T>
  try {
    r = await p
  } catch (e) {
    throw new ApiError('unreachable', `Cannot reach the lineup server (${(e as Error).message})`, 0)
  }
  if (!r.response.ok) {
    const body = r.error as { error?: { code?: string; message?: string } } | undefined
    throw new ApiError(
      body?.error?.code ?? 'internal',
      body?.error?.message ?? `HTTP ${r.response.status}`,
      r.response.status
    )
  }
  return r.data as T
}

export const api = {
  status: () => unwrap(client.GET('/status')),

  channels: (query?: { q?: string; group?: string; playlist?: string }) =>
    unwrap(client.GET('/channels', { params: { query } })),
  createChannel: (body: ChannelCreate) => unwrap(client.POST('/channels', { body })),
  patchChannel: (id: number, body: ChannelPatch) =>
    unwrap(client.PATCH('/channels/{id}', { params: { path: { id } }, body })),
  deleteChannel: (id: number) => unwrap(client.DELETE('/channels/{id}', { params: { path: { id } } })),
  resolve: (input: string) => unwrap(client.POST('/resolve', { body: { input } })),
  probe: (ids: number[], force = false) => unwrap(client.POST('/probe', { body: { ids, force } })),

  playlists: () => unwrap(client.GET('/playlists')),
  playlist: (slug: string) => unwrap(client.GET('/playlists/{slug}', { params: { path: { slug } } })),
  createPlaylist: (slug: string, name?: string) =>
    unwrap(client.POST('/playlists', { body: { slug, name: name || null } })),
  patchPlaylist: (slug: string, body: { name?: string; slug?: string }) =>
    unwrap(client.PATCH('/playlists/{slug}', { params: { path: { slug } }, body })),
  deletePlaylist: (slug: string) => unwrap(client.DELETE('/playlists/{slug}', { params: { path: { slug } } })),
  addItem: (slug: string, channel_id: number, position?: number) =>
    unwrap(
      client.POST('/playlists/{slug}/items', {
        params: { path: { slug } },
        body: { channel_id, position: position ?? null }
      })
    ),
  replaceItems: (slug: string, channel_ids: number[]) =>
    unwrap(client.PUT('/playlists/{slug}/items', { params: { path: { slug } }, body: { channel_ids } })),
  removeItem: (slug: string, channel_id: number) =>
    unwrap(client.DELETE('/playlists/{slug}/items/{channel_id}', { params: { path: { slug, channel_id } } })),

  settings: () => unwrap(client.GET('/settings')),
  patchSettings: (body: SettingsPatch) => unwrap(client.PATCH('/settings', { body })),
  channelInfo: (id: number, name = false, signal?: AbortSignal) =>
    unwrap(client.GET('/channels/{id}/info', { params: { path: { id }, query: { name } }, signal })),

  integrations: () => unwrap(client.GET('/integrations')),
  patchIntegration: (kind: SourceKind, body: IntegrationPatch) =>
    unwrap(client.PATCH('/integrations/{kind}', { params: { path: { kind } }, body })),
  testIntegration: (kind: SourceKind, component: Component, url?: string) =>
    unwrap(
      client.POST('/integrations/{kind}/test', { params: { path: { kind } }, body: { component, url: url ?? null } })
    ),
  integrationStatus: (kind: SourceKind, signal?: AbortSignal) =>
    unwrap(client.GET('/integrations/{kind}/status', { params: { path: { kind } }, signal })),
  importM3u: (m3u_text: string, playlist_slug: string, dry_run: boolean) =>
    unwrap(client.POST('/import', { body: { m3u_text, playlist_slug, dry_run } })),
  refresh: (slugs: string[] = []) => unwrap(client.POST('/refresh', { body: { slugs } })),

  epgSources: () => unwrap(client.GET('/epg/sources')),
  createEpgSource: (body: EpgSourceCreate) => unwrap(client.POST('/epg/sources', { body })),
  patchEpgSource: (id: number, body: EpgSourcePatch) =>
    unwrap(client.PATCH('/epg/sources/{id}', { params: { path: { id } }, body })),
  deleteEpgSource: (id: number) => unwrap(client.DELETE('/epg/sources/{id}', { params: { path: { id } } })),
  fetchEpg: (source_ids: number[] = []) => unwrap(client.POST('/epg/fetch', { body: { source_ids } })),
  hooks: () => unwrap(client.GET('/hooks')),
  createHook: (body: HookCreate) => unwrap(client.POST('/hooks', { body })),
  patchHook: (id: number, body: HookPatch) => unwrap(client.PATCH('/hooks/{id}', { params: { path: { id } }, body })),
  deleteHook: (id: number) => unwrap(client.DELETE('/hooks/{id}', { params: { path: { id } } })),
  testHook: (id: number) => unwrap(client.POST('/hooks/{id}/test', { params: { path: { id } } })),
  hookDeliveries: (id: number, limit = 50) =>
    unwrap(client.GET('/hooks/{id}/deliveries', { params: { path: { id }, query: { limit } } })),

  epgChannels: (q?: string, limit?: number) =>
    unwrap(client.GET('/epg/channels', { params: { query: { q: q || undefined, limit } } }))
}

/** Absolute URL of a playlist, as players and media servers are configured with it. */
export function playlistUrl(slug: string): string {
  return `${window.location.origin}/p/${encodeURIComponent(slug)}.m3u`
}

/** Absolute URL of a playlist's XMLTV guide. */
export function guideUrl(slug: string): string {
  return `${window.location.origin}/p/${encodeURIComponent(slug)}/epg.xml`
}

export function sourceLabel(s: Source): string {
  switch (s.kind) {
    case 'acestream':
      return `ace ${s.id.slice(0, 8)}`
    case 'vk':
      return `vk ${s.owner_id}_${s.video_id}`
    case 'direct':
      return s.url
  }
}

/** Display names of the integrations. */
export const INTEGRATION_NAMES: Record<SourceKind, string> = {
  acestream: 'Acestream',
  vk: 'VK proxy',
  direct: 'Direct URLs'
}

export function integrationName(kind: SourceKind): string {
  return INTEGRATION_NAMES[kind]
}
