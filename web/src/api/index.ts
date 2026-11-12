import createClient from 'openapi-fetch'
import type { components, paths } from './schema'

export type Channel = components['schemas']['Channel']
export type ChannelCreate = components['schemas']['ChannelCreate']
export type ChannelPatch = components['schemas']['ChannelPatch']
export type Playlist = components['schemas']['Playlist']
export type PlaylistDetail = components['schemas']['PlaylistDetail']
export type Settings = components['schemas']['Settings']
export type SettingsPatch = components['schemas']['SettingsPatch']
export type ImportReport = components['schemas']['ImportReport']
export type ResolveResponse = components['schemas']['ResolveResponse']
export type Status = components['schemas']['Status']
export type Route = components['schemas']['Route']
export type RouteConfig = components['schemas']['RouteConfig']
export type RouteCreate = components['schemas']['RouteCreate']
export type RoutePatch = components['schemas']['RoutePatch']
export type RouteStatus = components['schemas']['RouteStatus']
export type RouteTestResult = components['schemas']['RouteTestResult']
export type RuleSummary = components['schemas']['RuleSummary']
export type ResyncReport = components['schemas']['ResyncReport']
export type ChannelInfo = components['schemas']['ChannelInfo']
export type Observation = components['schemas']['Observation']
export type ContentState = components['schemas']['ContentState']
export type CheckOutcome = components['schemas']['CheckOutcome']
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

  channels: (query?: { q?: string; group?: string; playlist?: string; route_id?: number }) =>
    unwrap(client.GET('/channels', { params: { query } })),
  createChannel: (body: ChannelCreate) => unwrap(client.POST('/channels', { body })),
  patchChannel: (id: number, body: ChannelPatch) =>
    unwrap(client.PATCH('/channels/{id}', { params: { path: { id } }, body })),
  deleteChannel: (id: number) => unwrap(client.DELETE('/channels/{id}', { params: { path: { id } } })),
  resolve: (route_id: number, input: string) => unwrap(client.POST('/resolve', { body: { route_id, input } })),
  check: (ids: number[], force = false) => unwrap(client.POST('/checks', { body: { ids, force } })),

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
  channelInfo: (id: number, signal?: AbortSignal) =>
    unwrap(client.GET('/channels/{id}/info', { params: { path: { id } }, signal })),

  routes: () => unwrap(client.GET('/routes')),
  createRoute: (body: RouteCreate) => unwrap(client.POST('/routes', { body })),
  patchRoute: (id: number, body: RoutePatch) => unwrap(client.PATCH('/routes/{id}', { params: { path: { id } }, body })),
  deleteRoute: (id: number) => unwrap(client.DELETE('/routes/{id}', { params: { path: { id } } })),
  testRoute: (config: RouteConfig, input?: string) =>
    unwrap(client.POST('/routes/test', { body: { config, input: input || null } })),
  routeStatus: (id: number, signal?: AbortSignal) =>
    unwrap(client.GET('/routes/{id}/status', { params: { path: { id } }, signal })),
  resyncRoute: (id: number) => unwrap(client.POST('/routes/{id}/resync', { params: { path: { id } } })),
  importM3u: (m3u_text: string, playlist_slug: string, route_id: number, dry_run: boolean) =>
    unwrap(client.POST('/import', { body: { m3u_text, playlist_slug, route_id, dry_run } })),
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

/** A short label for a channel's source: its route and identity. */
export function sourceLabel(c: Pick<Channel, 'route_name' | 'identity'>): string {
  return `${c.route_name}: ${c.identity}`
}

/** Display text of a content state. */
export const CONTENT_STATES: Record<ContentState, string> = {
  live: 'Live',
  ended_processing: 'Ended, recording not ready',
  recording: 'Recording'
}

/** Short description of a route's config. */
export function routeSummary(c: RouteConfig): string {
  return c.kind === 'direct' ? 'Direct URLs' : `${c.rule} on ${c.base_url}`
}
