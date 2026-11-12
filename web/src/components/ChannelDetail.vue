<script setup lang="ts">
import { useToast } from '@nuxt/ui/composables'
import { computed, ref } from 'vue'
import { api, CONTENT_STATES, sourceLabel, type Channel } from '../api'
import { copy, liveEpoch, playerLinks, useAction, useInvalidateAll, usePolling, useRoutes } from '../composables'

/**
 * What lineup knows about one channel: its stored fields, what its route's gateway observes now
 * (for example whether it is watched, live or a recording), and its latest check. Opening it never
 * starts a stream; "Check now" asks the gateway to check, which may.
 */
const props = defineProps<{ channel: Channel | null }>()
const open = defineModel<boolean>('open', { required: true })
const emit = defineEmits<{ edit: [Channel] }>()

const toast = useToast()
const run = useAction()
const invalidate = useInvalidateAll()
const { data: routes } = useRoutes()
const route = computed(() => routes.value?.find((r) => r.id === props.channel?.route_id))
const isGateway = computed(() => route.value?.config.kind === 'gateway')

/**
 * What the live answers describe: the open channel, its source, its route's revision, and the event
 * stream's epoch (reconnects and resyncs). A change drops answers in flight and asks again.
 */
const target = computed(() => {
  const c = props.channel
  if (!open.value || !c || !isGateway.value) return undefined
  return { id: c.id, key: `${c.id}|${c.route_id}|${c.source}|${route.value?.revision}|${liveEpoch.value}` }
})

/** The gateway's observation, every 5 s while open and visible. */
const live = usePolling(
  () => target.value?.key,
  (_, signal) => api.channelInfo(target.value!.id, signal),
  5000
)

const info = computed(() => live.data.value)
const observation = computed(() => info.value?.observation ?? null)
const checks = computed(() => info.value?.checks)

const when = (t: string) => new Date(t).toLocaleString()
const outcomeColor = (o: string | null | undefined) =>
  o === 'ok' ? ('success' as const) : o === 'dead' ? ('error' as const) : ('neutral' as const)

const checking = ref(false)
async function checkNow() {
  const c = props.channel
  if (!c) return
  checking.value = true
  const r = await run(() => api.check([c.id]))
  checking.value = false
  if (!r.ok) return
  await invalidate()
  void live.refresh()
  const res = r.value.results[0]
  if (!res) return
  const d = res.detail as { error?: string } | null
  const color = res.outcome === 'ok' ? 'success' : res.outcome === 'dead' ? 'error' : 'warning'
  toast.add({ title: `${c.name}: ${res.outcome}`, description: res.reason ?? d?.error ?? undefined, color })
}

const links = computed(() => (props.channel ? playerLinks(props.channel.play_url) : null))
</script>

<template>
  <USlideover v-model:open="open" :title="channel?.name ?? 'Channel'" :ui="{ content: 'sm:max-w-lg' }">
    <template #description>
      <span v-if="channel" class="flex flex-wrap items-center gap-1.5">
        <span>{{ channel.route_name }}</span>
        <UBadge v-if="!channel.enabled" label="disabled" color="neutral" variant="subtle" size="sm" />
        <UBadge v-if="channel.is_event" label="event" color="warning" variant="subtle" size="sm" />
        <UBadge v-if="!channel.route_enabled" label="route disabled" color="neutral" variant="outline" size="sm" />
      </span>
    </template>

    <template #body>
      <div v-if="channel" class="flex flex-col gap-6">
        <!-- Overview -->
        <section class="flex flex-col gap-3">
          <h3 class="text-sm font-semibold text-muted uppercase tracking-wide">Overview</h3>
          <dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm">
            <dt class="text-muted">Route</dt>
            <dd>
              <ULink to="/routes" class="underline">{{ channel.route_name }}</ULink>
              <span v-if="!channel.route_enabled" class="text-muted">
                (disabled: this channel is left out of playlists and guides)
              </span>
            </dd>
            <dt class="text-muted">Source</dt>
            <dd class="font-mono break-all">{{ channel.source }}</dd>
            <template v-if="channel.identity !== channel.source">
              <dt class="text-muted">Identity</dt>
              <dd class="font-mono break-all">{{ sourceLabel(channel) }}</dd>
            </template>
            <dt class="text-muted">Plays from</dt>
            <dd class="break-all">{{ channel.play_url }}</dd>
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
          <div v-if="links" class="flex flex-wrap gap-2">
            <UButton
              icon="i-lucide-clipboard"
              color="neutral"
              variant="subtle"
              label="Copy URL"
              @click="copy(channel.play_url, toast)"
            />
            <UButton icon="i-lucide-play" color="neutral" variant="subtle" label="VLC" :href="links.vlc" />
            <UButton icon="i-lucide-play" color="neutral" variant="subtle" label="Infuse" :href="links.infuse" />
          </div>
        </section>

        <!-- What the gateway sees now -->
        <section v-if="isGateway" class="flex flex-col gap-3">
          <h3 class="text-sm font-semibold text-muted uppercase tracking-wide">Now</h3>
          <!-- A failed poll hides an older answer: it may no longer be true. -->
          <div v-if="live.error.value" class="text-sm text-error">Unavailable: {{ live.error.value }}</div>
          <p v-else-if="info?.observation_error" class="text-sm text-muted">{{ info.observation_error }}</p>
          <template v-else-if="observation">
            <div class="flex flex-wrap items-center gap-2 text-sm">
              <UBadge
                v-if="observation.content_state"
                :label="CONTENT_STATES[observation.content_state]"
                :color="observation.content_state === 'live' ? 'success' : 'neutral'"
                icon="i-lucide-clapperboard"
                variant="subtle"
              />
              <UBadge
                v-if="observation.watching != null"
                :label="observation.watching ? 'Watched now' : 'Not watched'"
                :color="observation.watching ? 'success' : 'neutral'"
                :icon="observation.watching ? 'i-lucide-radio' : 'i-lucide-circle-pause'"
                variant="subtle"
              />
              <span v-if="!observation.available" class="text-muted">Nothing to observe</span>
            </div>
            <p v-if="observation.title" class="text-sm">{{ observation.title }}</p>
            <p v-if="observation.reason" class="text-sm text-muted">{{ observation.reason }}</p>
            <dl v-if="observation.facts.length" class="grid grid-cols-2 sm:grid-cols-3 gap-3 text-sm" aria-live="polite">
              <div v-for="f in observation.facts" :key="f.label" class="rounded-md bg-elevated p-3">
                <dt class="text-xs text-muted">{{ f.label }}</dt>
                <dd class="text-lg font-semibold tabular-nums break-all">{{ f.value }}</dd>
              </div>
            </dl>
          </template>
          <p v-else-if="info" class="text-sm text-muted">The gateway's rule does not report anything to observe.</p>
          <p v-else class="text-sm text-muted">Loading…</p>
        </section>

        <!-- Health -->
        <section v-if="isGateway" class="flex flex-col gap-3">
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
              <span v-if="channel.health.reason" class="text-muted">({{ channel.health.reason }})</span>
            </dd>
            <template v-if="channel.health.content_state">
              <dt class="text-muted">Content</dt>
              <dd>{{ CONTENT_STATES[channel.health.content_state] }}</dd>
            </template>
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
              :disabled="checks?.available !== true"
              @click="checkNow"
            />
            <span v-if="checks && !checks.available" class="text-sm text-muted">{{ checks.reason }}</span>
            <span v-else class="text-xs text-dimmed">The gateway checks it; it skips channels someone is watching.</span>
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
