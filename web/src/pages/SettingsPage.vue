<script setup lang="ts">
import { useQuery, useQueryCache } from '@pinia/colada'
import { useToast } from '@nuxt/ui/composables'
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { onBeforeRouteLeave } from 'vue-router'
import { api } from '../api'
import { useAction } from '../composables'
import SettingsCard from '../components/settings/SettingsCard.vue'
import { hoursError, useSettingsSection } from '../components/settings/section'

const run = useAction()
const toast = useToast()
const cache = useQueryCache()
const { data: settings, error, refetch } = useQuery({ key: ['settings'], query: () => api.settings() })
const { data: status } = useQuery({ key: ['status'], query: () => api.status() })
const {
  data: sources,
  error: sourcesError,
  refetch: refetchSources
} = useQuery({ key: ['epg-sources'], query: () => api.epgSources() })

const guide = useSettingsSection(['epg_refresh_hours'], settings, {
  validate: (d) => hoursError(d.epg_refresh_hours)
})

/** Edit the draft (kept out of the template: vue-tsc mistypes deep v-model targets). */
function setDraft(section: { draft: object }, key: string, value: string | number) {
  ;(section.draft as Record<string, unknown>)[key] = value
}

const loading = computed(() => !settings.value)

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

// ---------------------------------------------------------------- actions

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

onBeforeRouteLeave(() => !guide.dirty || window.confirm('Discard unsaved settings?'))
const onUnload = (e: BeforeUnloadEvent) => e.preventDefault()
watch(
  () => guide.dirty,
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

        <p class="text-sm text-muted">
          Service addresses (the Acestream engine, acexy, the VK proxy) are set on the
          <ULink to="/integrations" class="underline">Integrations</ULink> page.
        </p>

        <section class="flex flex-col gap-4">
          <h2 class="text-lg font-semibold">Guide</h2>
          <UAlert
            v-if="sourcesError && !sources"
            color="warning"
            variant="subtle"
            title="Could not load guide sources"
            :actions="[{ label: 'Retry', onClick: () => refetchSources() }]"
          />
          <SettingsCard
            title="Guide data"
            icon="i-lucide-calendar-range"
            :description="guideSummary"
            :section="guide"
            :loading="loading"
          >
            <UFormField
              label="Refresh every (hours)"
              help="Sources are fetched 3 minutes after start, then at this interval. 0 turns the schedule off."
            >
              <UInput
                :model-value="guide.draft.epg_refresh_hours"
                type="number"
                min="0"
                max="168"
                class="w-32"
                size="lg"
                @update:model-value="(v: unknown) => setDraft(guide, 'epg_refresh_hours', Number(v))"
              />
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
                :disabled="guide.dirty || loading || !sources?.length"
                @click="fetchAll"
              />
              <UButton
                color="neutral"
                variant="ghost"
                size="lg"
                class="min-h-11"
                label="Manage sources"
                trailing-icon="i-lucide-arrow-right"
                to="/epg"
              />
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
