<script setup lang="ts">
import { useToast } from '@nuxt/ui/composables'
import { computed, ref, watch } from 'vue'
import { api, integrationName, sourceLabel, type AcestreamLive, type Channel } from '../api'
import { copy, playerLinks, useAction, useInvalidateAll, usePolling } from '../composables'

/**
 * What lineup knows about one channel: its stored fields, its integration's live view (for
 * acestream: the engine's name, and the numbers of the session acexy has open while someone
 * watches), and its latest check. Opening it never starts a stream; "Check now" does, briefly.
 */
const props = defineProps<{ channel: Channel | null }>()
const open = defineModel<boolean>('open', { required: true })
const emit = defineEmits<{ edit: [Channel] }>()

const toast = useToast()
const run = useAction()
const invalidate = useInvalidateAll()

const id = computed(() => (open.value ? props.channel?.id : undefined))
const isAce = computed(() => props.channel?.source.kind === 'acestream')

/** Live numbers, every 5 s while open and visible (no engine name: that is fetched once below). */
const live = usePolling(() => id.value, (cid) => api.channelInfo(cid, false), 5000)

/** The engine's name for the content id, once per opening. */
const engineName = ref<{ name?: string | null; error?: string | null } | null>(null)
let nameSeq = 0
watch(
  [id, isAce],
  async ([cid, ace]) => {
    const mine = ++nameSeq
    engineName.value = null
    if (cid === undefined || !ace) return
    try {
      const info = await api.channelInfo(cid, true)
      if (mine === nameSeq)
        engineName.value = { name: info.acestream?.engine_name, error: info.acestream?.engine_name_error }
    } catch (e) {
      if (mine === nameSeq) engineName.value = { error: (e as Error).message }
    }
  },
  { immediate: true }
)

const info = computed(() => (live.data.value?.channel_id === props.channel?.id ? live.data.value : undefined))
const acestream = computed(() => info.value?.acestream ?? null)
const checks = computed(() => info.value?.checks)

function liveState(l: AcestreamLive) {
  switch (l.state) {
    case 'active':
      return { label: 'Watched now', color: 'success' as const, icon: 'i-lucide-radio' }
    case 'inactive':
      return { label: 'Not watched', color: 'neutral' as const, icon: 'i-lucide-circle-pause' }
    default:
      return { label: 'Unknown', color: 'neutral' as const, icon: 'i-lucide-circle-help' }
  }
}

const num = (v: number | null | undefined) => (v == null ? '—' : v.toLocaleString())
function bytes(v: number | null | undefined) {
  if (v == null) return '—'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let n = v
  let i = 0
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024
    i++
  }
  return `${n.toFixed(n >= 10 || i === 0 ? 0 : 1)} ${units[i]}`
}
const when = (t: string) => new Date(t).toLocaleString()
const outcomeColor = (o: string | null | undefined) =>
  o === 'ok' ? ('success' as const) : o === 'dead' ? ('error' as const) : ('neutral' as const)

const checking = ref(false)
async function checkNow() {
  const c = props.channel
  if (!c) return
  checking.value = true
  const r = await run(() => api.probe([c.id]))
  checking.value = false
  if (!r.ok) return
  await invalidate()
  void live.refresh()
  const res = r.value.results[0]
  if (!res) return
  const d = res.detail as { error?: string; reason?: string } | null
  const color = res.outcome === 'ok' ? 'success' : res.outcome === 'dead' ? 'error' : 'warning'
  toast.add({ title: `${c.name}: ${res.outcome}`, description: d?.error ?? d?.reason, color })
}

const links = computed(() => (props.channel?.stream_url ? playerLinks(props.channel.stream_url) : null))
</script>

<template>
  <USlideover v-model:open="open" :title="channel?.name ?? 'Channel'" :ui="{ content: 'sm:max-w-lg' }">
    <template #description>
      <span v-if="channel" class="flex flex-wrap items-center gap-1.5">
        <span>{{ integrationName(channel.source.kind) }}</span>
        <UBadge v-if="!channel.enabled" label="disabled" color="neutral" variant="subtle" size="sm" />
        <UBadge v-if="channel.is_event" label="event" color="warning" variant="subtle" size="sm" />
        <UBadge v-if="!channel.configured" label="not set up" color="neutral" variant="outline" size="sm" />
      </span>
    </template>

    <template #body>
      <div v-if="channel" class="flex flex-col gap-6">
        <!-- Overview -->
        <section class="flex flex-col gap-3">
          <h3 class="text-sm font-semibold text-muted uppercase tracking-wide">Overview</h3>
          <dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm">
            <dt class="text-muted">Source</dt>
            <dd class="font-mono break-all">{{ sourceLabel(channel.source) }}</dd>
            <dt class="text-muted">Stream</dt>
            <dd class="break-all">
              <template v-if="channel.stream_url">{{ channel.stream_url }}</template>
              <span v-else class="text-muted">
                None: the {{ integrationName(channel.source.kind) }} integration is not set up, so this channel is
                left out of playlists and guides.
                <ULink to="/integrations" class="underline">Set it up</ULink>
              </span>
            </dd>
            <dt class="text-muted">Playlists</dt>
            <dd class="flex flex-wrap gap-1">
              <template v-if="channel.playlists.length">
                <UBadge
                  v-for="p in channel.playlists"
                  :key="p"
                  :label="p"
                  variant="subtle"
                  size="sm"
                  :to="`/playlists/${encodeURIComponent(p)}`"
                />
              </template>
              <span v-else class="text-muted">In no playlist</span>
            </dd>
            <dt class="text-muted">Group</dt>
            <dd>{{ channel.group_title ?? '—' }}</dd>
            <dt class="text-muted">Guide</dt>
            <dd class="break-all">
              <template v-if="channel.tvg_id">{{ channel.tvg_id }}</template>
              <span v-else-if="channel.is_event" class="text-muted">Event placeholder (no guide id)</span>
              <span v-else class="text-muted">No guide id</span>
            </dd>
            <template v-if="channel.is_event">
              <dt class="text-muted">Kickoff</dt>
              <dd>{{ channel.kickoff_at ? when(channel.kickoff_at) : '—' }}</dd>
            </template>
          </dl>
          <div v-if="channel.stream_url && links" class="flex flex-wrap gap-2">
            <UButton
              icon="i-lucide-clipboard"
              color="neutral"
              variant="subtle"
              label="Copy URL"
              @click="copy(channel.stream_url, toast)"
            />
            <UButton icon="i-lucide-play" color="neutral" variant="subtle" label="VLC" :href="links.vlc" />
            <UButton icon="i-lucide-play" color="neutral" variant="subtle" label="Infuse" :href="links.infuse" />
          </div>
        </section>

        <!-- Acestream -->
        <section v-if="isAce" class="flex flex-col gap-3">
          <h3 class="text-sm font-semibold text-muted uppercase tracking-wide">Acestream</h3>
          <dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm">
            <dt class="text-muted">Engine name</dt>
            <dd class="break-all">
              <template v-if="engineName?.name">{{ engineName.name }}</template>
              <span v-else-if="engineName?.error" class="text-muted">{{ engineName.error }}</span>
              <span v-else class="text-muted">Asking the engine…</span>
            </dd>
          </dl>

          <div v-if="live.error.value && !acestream" class="text-sm text-error">
            Live numbers unavailable: {{ live.error.value }}
          </div>
          <template v-else-if="acestream">
            <div class="flex flex-wrap items-center gap-2 text-sm">
              <UBadge
                :label="liveState(acestream.live).label"
                :color="liveState(acestream.live).color"
                :icon="liveState(acestream.live).icon"
                variant="subtle"
              />
              <span v-if="acestream.live.state === 'active' && acestream.live.clients != null" class="text-muted">
                {{ acestream.live.clients }} {{ acestream.live.clients === 1 ? 'player' : 'players' }} through acexy
              </span>
            </div>
            <p v-if="acestream.live.reason" class="text-sm text-muted">{{ acestream.live.reason }}</p>
            <dl
              v-if="acestream.live.state === 'active'"
              class="grid grid-cols-2 sm:grid-cols-3 gap-3 text-sm"
              aria-live="polite"
            >
              <div class="rounded-md bg-elevated p-3">
                <dt class="text-xs text-muted">Peers</dt>
                <dd class="text-lg font-semibold tabular-nums">{{ num(acestream.live.peers) }}</dd>
              </div>
              <div class="rounded-md bg-elevated p-3">
                <dt class="text-xs text-muted">Down</dt>
                <dd class="text-lg font-semibold tabular-nums">{{ num(acestream.live.speed_down) }} <span class="text-xs font-normal text-muted">KB/s</span></dd>
              </div>
              <div class="rounded-md bg-elevated p-3">
                <dt class="text-xs text-muted">Up</dt>
                <dd class="text-lg font-semibold tabular-nums">{{ num(acestream.live.speed_up) }} <span class="text-xs font-normal text-muted">KB/s</span></dd>
              </div>
              <div class="rounded-md bg-elevated p-3">
                <dt class="text-xs text-muted">Status</dt>
                <dd class="text-lg font-semibold">{{ acestream.live.status ?? '—' }}</dd>
              </div>
              <div class="rounded-md bg-elevated p-3">
                <dt class="text-xs text-muted">Downloaded</dt>
                <dd class="text-lg font-semibold tabular-nums">{{ bytes(acestream.live.downloaded) }}</dd>
              </div>
              <div class="rounded-md bg-elevated p-3">
                <dt class="text-xs text-muted">Uploaded</dt>
                <dd class="text-lg font-semibold tabular-nums">{{ bytes(acestream.live.uploaded) }}</dd>
              </div>
            </dl>
            <p v-else class="text-sm text-muted">
              Numbers appear while someone watches through acexy; opening this never starts a stream.
            </p>
          </template>
          <p v-else class="text-sm text-muted">Loading…</p>
        </section>

        <!-- Health -->
        <section v-if="isAce" class="flex flex-col gap-3">
          <h3 class="text-sm font-semibold text-muted uppercase tracking-wide">Health</h3>
          <dl v-if="channel.health" class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm">
            <dt class="text-muted">Last check</dt>
            <dd class="flex flex-wrap items-center gap-2">
              <UBadge
                :label="channel.health.last_attempt"
                :color="outcomeColor(channel.health.last_attempt)"
                variant="subtle"
                size="sm"
              />
              <span class="text-muted">{{ when(channel.health.last_attempt_at) }}</span>
            </dd>
            <template v-if="channel.health.conclusive && channel.health.conclusive !== channel.health.last_attempt">
              <dt class="text-muted">Last known</dt>
              <dd class="flex flex-wrap items-center gap-2">
                <UBadge
                  :label="channel.health.conclusive"
                  :color="outcomeColor(channel.health.conclusive)"
                  variant="subtle"
                  size="sm"
                />
                <span v-if="channel.health.conclusive_at" class="text-muted">{{ when(channel.health.conclusive_at) }}</span>
              </dd>
            </template>
          </dl>
          <p v-else class="text-sm text-muted">Never checked.</p>
          <div class="flex flex-wrap items-center gap-3">
            <UButton
              icon="i-lucide-activity"
              color="neutral"
              variant="subtle"
              label="Check now"
              :loading="checking"
              :disabled="checks?.available === false"
              @click="checkNow"
            />
            <span v-if="checks && !checks.available" class="text-sm text-muted">{{ checks.reason }}</span>
            <span v-else class="text-xs text-dimmed">Starts a short engine session; skipped while someone watches.</span>
          </div>
        </section>
      </div>
    </template>

    <template #footer>
      <div class="flex justify-end gap-2 w-full">
        <UButton color="neutral" variant="ghost" label="Close" @click="open = false" />
        <UButton icon="i-lucide-pencil" label="Edit" :disabled="!channel" @click="channel && emit('edit', channel)" />
      </div>
    </template>
  </USlideover>
</template>
