<script setup lang="ts">
import { useQuery, useQueryCache } from '@pinia/colada'
import { useToast } from '@nuxt/ui/composables'
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { onBeforeRouteLeave } from 'vue-router'
import { api } from '../api'
import { useAction, useChannels } from '../composables'
import BaseUrlField from '../components/settings/BaseUrlField.vue'
import SettingsCard from '../components/settings/SettingsCard.vue'
import { baseUrlError, useSettingsSection } from '../components/settings/section'

const run = useAction()
const toast = useToast()
const cache = useQueryCache()
const { data: settings, error, refetch } = useQuery({ key: ['settings'], query: () => api.settings() })
const { data: status } = useQuery({ key: ['status'], query: () => api.status() })
const { data: channels, error: channelsError, refetch: refetchChannels } = useChannels()
const {
  data: sources,
  error: sourcesError,
  refetch: refetchSources
} = useQuery({ key: ['epg-sources'], query: () => api.epgSources() })

const reload = () => refetch().then((r) => r.data)
const hours = (v: number) => (Number.isInteger(v) && v >= 0 && v <= 168 ? null : 'Enter whole hours from 0 to 168')
const cards = {
  acexy: useSettingsSection(['acexy_base'], settings, { reload, validate: (d) => baseUrlError(d.acexy_base) }),
  engine: useSettingsSection(['engine_base', 'probe_interval_hours'], settings, {
    reload,
    validate: (d) => baseUrlError(d.engine_base) ?? hours(d.probe_interval_hours)
  }),
  vk: useSettingsSection(['vk_base'], settings, { reload, validate: (d) => baseUrlError(d.vk_base) }),
  guide: useSettingsSection(['epg_refresh_hours'], settings, { reload, validate: (d) => hours(d.epg_refresh_hours) })
}
const sections = Object.values(cards)

/** The three service cards share one layout. */
const sourceCards = computed(() => [
  {
    target: 'acexy' as const,
    field: 'acexy_base',
    section: cards.acexy,
    title: 'acexy',
    icon: 'i-lucide-radio',
    description: channelsLine('acestream'),
    help: 'Acestream channels play from {base}/ace/getstream?id=…; health checks also ask it whether a stream is being watched.',
    placeholder: 'http://media-host:8080'
  },
  {
    target: 'engine' as const,
    field: 'engine_base',
    section: cards.engine,
    title: 'Acestream engine',
    icon: 'i-lucide-cpu',
    description: 'Looks up channel names when you add one, and runs health checks. Players never connect to it.',
    help: 'Reachable from the lineup server.',
    placeholder: 'http://media-host:6878'
  },
  {
    target: 'vk' as const,
    field: 'vk_base',
    section: cards.vk,
    title: 'VK proxy',
    icon: 'i-lucide-video',
    description: channelsLine('vk'),
    help: 'VK channels play from {base}/vk/{owner}_{video}[?list=…].',
    placeholder: 'http://media-host:8099'
  }
])

function draftOf(section: { draft: object }, key: string): string {
  return String((section.draft as Record<string, unknown>)[key] ?? '')
}

/** Edit a card's draft (kept out of the template: vue-tsc mistypes deep v-model targets). */
function setDraft(section: { draft: object }, key: string, value: string | number) {
  ;(section.draft as Record<string, unknown>)[key] = value
}

const loading = computed(() => !settings.value)
const anyDirty = computed(() => sections.some((s) => s.dirty))

/** Other pages (playlist URLs, channel stream URLs) depend on the base URLs. */
function saved() {
  cache.invalidateQueries({ key: ['channels'] })
  cache.invalidateQueries({ key: ['playlist'] })
}

const count = (kind: string) => (channels.value ?? []).filter((c) => c.source.kind === kind).length
const channelsLine = (kind: string) => {
  if (channelsError.value && !channels.value) return 'Channel count unavailable.'
  if (!channels.value) return undefined
  const n = count(kind)
  return n === 1 ? 'Used by 1 channel.' : `Used by ${n} channels.`
}
const guideSummary = computed(() => {
  if (!sources.value) return sourcesError.value ? 'Could not load guide sources.' : 'Loading guide sources…'
  const list = sources.value
  if (!list.length) return 'No guide sources yet.'
  const enabled = list.filter((s) => s.enabled)
  const parts = [
    [enabled.filter((s) => s.last_status === 'ok' || s.last_status === 'not_modified').length, 'working'],
    [enabled.filter((s) => s.last_status === 'error').length, 'failing'],
    [enabled.filter((s) => !s.last_status).length, 'not fetched yet'],
    [list.length - enabled.length, 'disabled']
  ] as const
  const detail = parts.filter(([n]) => n > 0).map(([n, what]) => `${n} ${what}`)
  return `${list.length} source${list.length > 1 ? 's' : ''}: ${detail.join(', ')}.`
})

// ---------------------------------------------------------------- actions on saved settings

const probing = ref(false)
async function checkAll() {
  probing.value = true
  await run(() => api.probe([]), 'Checking all acestream channels in the background')
  probing.value = false
}

const fetching = ref(false)
async function fetchAll() {
  fetching.value = true
  const r = await run(() => api.fetchEpg([]))
  fetching.value = false
  cache.invalidateQueries({ key: ['epg-sources'] })
  if (!r.ok) return
  const failed = r.value.filter((x) => x.status === 'error').length
  toast.add(
    failed
      ? { title: `${failed} source${failed > 1 ? 's' : ''} failed`, color: 'error', icon: 'i-lucide-circle-x' }
      : { title: 'Guide sources fetched', color: 'success', icon: 'i-lucide-check' }
  )
}

// ---------------------------------------------------------------- unsaved changes

onBeforeRouteLeave(() => !anyDirty.value || window.confirm('Discard unsaved settings?'))
const onUnload = (e: BeforeUnloadEvent) => e.preventDefault()
watch(
  anyDirty,
  (d) => (d ? window.addEventListener('beforeunload', onUnload) : window.removeEventListener('beforeunload', onUnload)),
  { immediate: true }
)
onBeforeUnmount(() => window.removeEventListener('beforeunload', onUnload))
</script>

<template>
  <UDashboardPanel id="settings">
    <template #header>
      <UDashboardNavbar title="Settings">
        <template #leading>
          <UDashboardSidebarCollapse />
        </template>
      </UDashboardNavbar>
    </template>

    <template #body>
      <div class="flex flex-col gap-8 max-w-2xl w-full">
        <UAlert
          v-if="error && !settings"
          color="error"
          variant="subtle"
          icon="i-lucide-circle-x"
          :title="`Could not load settings: ${error.message}`"
          :actions="[{ label: 'Retry', onClick: () => refetch() }]"
        />

        <section class="flex flex-col gap-4">
          <div>
            <h2 class="text-lg font-semibold">Sources</h2>
            <UAlert
              v-if="channelsError && !channels"
              class="my-2"
              color="warning"
              variant="subtle"
              title="Could not load channels, so channel counts are missing"
              :actions="[{ label: 'Retry', onClick: () => refetchChannels() }]"
            />
            <p class="text-sm text-muted">
              Playlists send Jellyfin and your players to acexy and the VK proxy, so use addresses they can reach,
              not localhost. Only the lineup server talks to the Acestream engine.
            </p>
          </div>

          <SettingsCard
            v-for="c in sourceCards"
            :key="c.target"
            :title="c.title"
            :icon="c.icon"
            :description="c.description"
            :section="c.section"
            :loading="loading"
            @saved="saved"
          >
            <BaseUrlField
              :model-value="draftOf(c.section, c.field)"
              :label="'Base URL'"
              :help="c.help"
              :target="c.target"
              :placeholder="c.placeholder"
              :error="baseUrlError(draftOf(c.section, c.field))"
              @update:model-value="(v: string) => setDraft(c.section, c.field, v)"
            />
            <UFormField
              v-if="c.target === 'engine'"
              label="Health checks every (hours)"
              help="Checks each acestream channel's swarm, skipping streams someone is watching. 0 turns the schedule off; you can still check by hand."
            >
              <UInput
                :model-value="cards.engine.draft.probe_interval_hours"
                type="number"
                min="0"
                max="168"
                class="w-32"
                size="lg"
                @update:model-value="(v: unknown) => setDraft(cards.engine, 'probe_interval_hours', Number(v))"
              />
            </UFormField>
            <template v-if="c.target === 'engine'" #actions>
              <UButton
                icon="i-lucide-activity"
                color="neutral"
                variant="subtle"
                size="lg"
                class="min-h-11"
                label="Check all now"
                :loading="probing"
                :disabled="cards.engine.dirty || loading"
                :title="cards.engine.dirty ? 'Save first: checks use the saved engine URL' : undefined"
                @click="checkAll"
              />
            </template>
          </SettingsCard>
        </section>

        <section class="flex flex-col gap-4">
          <h2 class="text-lg font-semibold">Guide</h2>
          <UAlert
            v-if="sourcesError && !sources"
            color="warning"
            variant="subtle"
            title="Could not load guide sources"
            :actions="[{ label: 'Retry', onClick: () => refetchSources() }]"
          />
          <SettingsCard title="Guide data" icon="i-lucide-calendar-range" :description="guideSummary" :section="cards.guide" :loading="loading">
            <UFormField
              label="Refresh every (hours)"
              help="Sources are fetched 3 minutes after start, then at this interval. 0 turns the schedule off."
            >
              <UInput :model-value="cards.guide.draft.epg_refresh_hours"
              @update:model-value="(v: unknown) => setDraft(cards.guide, 'epg_refresh_hours', Number(v))" type="number" min="0" max="168" class="w-32" size="lg" />
            </UFormField>
            <template #actions>
              <UButton
                icon="i-lucide-refresh-cw"
                color="neutral"
                variant="subtle"
                size="lg"
                class="min-h-11"
                label="Fetch all now"
                :loading="fetching"
                :disabled="cards.guide.dirty || loading || !sources?.length"
                @click="fetchAll"
              />
              <UButton color="neutral" variant="ghost" size="lg" class="min-h-11"
                label="Manage sources" trailing-icon="i-lucide-arrow-right" to="/epg" />
            </template>
          </SettingsCard>
        </section>

        <footer class="text-xs text-dimmed flex flex-wrap gap-x-4 gap-y-1">
          <span v-if="status">lineup {{ status.version }}</span>
          <a href="/api/openapi.json" class="underline" target="_blank" rel="noopener">API (OpenAPI)</a>
        </footer>
      </div>
    </template>
  </UDashboardPanel>
</template>
