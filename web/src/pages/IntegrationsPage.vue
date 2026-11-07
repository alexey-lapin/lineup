<script setup lang="ts">
import { useQuery, useQueryCache } from '@pinia/colada'
import { computed, onBeforeUnmount, reactive, ref, watch } from 'vue'
import { onBeforeRouteLeave } from 'vue-router'
import { api, type ConnectionTestResult, type Integration, type IntegrationStatus, type SourceKind } from '../api'
import { useAction, usePolling } from '../composables'
import BaseUrlField from '../components/settings/BaseUrlField.vue'
import StatusLine from '../components/settings/StatusLine.vue'
import SettingsCard from '../components/settings/SettingsCard.vue'
import { baseUrlError, hoursError, useSection } from '../components/settings/section'

const run = useAction()
const cache = useQueryCache()
const { data: list, error, refetch } = useQuery({ key: ['integrations'], query: () => api.integrations() })
const of = (kind: SourceKind) => computed(() => list.value?.find((i) => i.kind === kind))
const ace = of('acestream')
const vk = of('vk')
const direct = of('direct')

const loading = computed(() => !list.value)
/** Example addresses (kept out of the template: vue-tsc mangles "//" in some attribute values). */
const examples = { engine: 'http://media-host:6878', acexy: 'http://media-host:8080', vk: 'http://media-host:8099' }

// ---------------------------------------------------------------- cards

type AceDraft = { engine_url: string; acexy_url: string; probe_interval_hours: number }
const aceCard = useSection<Integration, AceDraft>(
  ace,
  (i) => {
    const c = i.config.kind === 'acestream' ? i.config : { engine_url: '', acexy_url: null, probe_interval_hours: 0 }
    return { engine_url: c.engine_url, acexy_url: c.acexy_url ?? '', probe_interval_hours: c.probe_interval_hours }
  },
  (d) =>
    api.patchIntegration('acestream', {
      kind: 'acestream',
      engine_url: d.engine_url.trim(),
      acexy_url: d.acexy_url.trim() || null,
      probe_interval_hours: d.probe_interval_hours
    }),
  { validate: (d) => baseUrlError(d.engine_url, false) ?? baseUrlError(d.acexy_url, false) ?? aceRules(d) }
)

/** Acestream rules beyond URL syntax (the URL fields show their own errors). */
function aceRules(d: AceDraft): string | null {
  if (!d.engine_url.trim() && d.acexy_url.trim()) return 'acexy needs the engine: set the engine URL too'
  return hoursError(d.probe_interval_hours)
}

type VkDraft = { url: string }
const vkCard = useSection<Integration, VkDraft>(
  vk,
  (i) => ({ url: i.config.kind === 'vk' ? i.config.url : '' }),
  (d) => api.patchIntegration('vk', { kind: 'vk', url: d.url.trim() }),
  { validate: (d) => baseUrlError(d.url, false) }
)

/** Playback URLs changed: channels and playlists show them. */
function saved() {
  cache.invalidateQueries({ key: ['channels'] })
  cache.invalidateQueries({ key: ['playlist'] })
}

/** Edit a card's draft (kept out of the template: vue-tsc mistypes deep v-model targets). */
function setDraft(section: { draft: object }, key: string, value: string | number) {
  ;(section.draft as Record<string, unknown>)[key] = value
}

// ---------------------------------------------------------------- live status (saved config)

/** Poll the status of a configured integration; a saved change (new revision) polls again at once. */
function useStatus(i: typeof ace) {
  return usePolling(
    () => (i.value?.configured ? `${i.value.kind}:${i.value.revision}` : undefined),
    () => api.integrationStatus(i.value!.kind),
    15000
  )
}
/** Live answers per integration (refs inside are unwrapped). */
const status = reactive({ ace: useStatus(ace), vk: useStatus(vk) })

function statusOf(s: IntegrationStatus | undefined, name: ConnectionTestResult['component']) {
  return s?.components.find((c) => c.component === name)
}

const channelsLine = (i: Integration | undefined) => {
  if (!i) return undefined
  return i.channel_count === 1 ? 'Used by 1 channel.' : `Used by ${i.channel_count} channels.`
}

const engineSet = computed(() => !!aceCard.draft.engine_url?.trim())
const acexySet = computed(() => !!aceCard.draft.acexy_url?.trim())

// ---------------------------------------------------------------- actions

const probing = ref(false)
async function checkAll() {
  probing.value = true
  await run(() => api.probe([]), 'Checking all acestream channels in the background')
  probing.value = false
}

// ---------------------------------------------------------------- unsaved changes

const anyDirty = computed(() => aceCard.dirty || vkCard.dirty)
onBeforeRouteLeave(() => !anyDirty.value || window.confirm('Discard unsaved changes?'))
const onUnload = (e: BeforeUnloadEvent) => e.preventDefault()
watch(
  anyDirty,
  (d) => (d ? window.addEventListener('beforeunload', onUnload) : window.removeEventListener('beforeunload', onUnload)),
  { immediate: true }
)
onBeforeUnmount(() => window.removeEventListener('beforeunload', onUnload))
</script>

<template>
  <UDashboardPanel id="integrations">
    <template #header>
      <UDashboardNavbar title="Integrations">
        <template #leading>
          <UDashboardSidebarCollapse />
        </template>
      </UDashboardNavbar>
    </template>

    <template #body>
      <div class="flex flex-col gap-6 max-w-2xl w-full">
        <UAlert
          v-if="error && !list"
          color="error"
          variant="subtle"
          icon="i-lucide-circle-x"
          :title="`Could not load integrations: ${error.message}`"
          :actions="[{ label: 'Retry', onClick: () => refetch() }]"
        />
        <p class="text-sm text-muted">
          The services your channels play through. Playlists send players to acexy (or the engine, without
          acexy) and to the VK proxy, so use addresses your players and media server can reach, not localhost.
          Channels of an integration that is not set up stay listed but are left out of playlists and guides.
        </p>

        <!-- Acestream -->
        <SettingsCard
          title="Acestream"
          icon="i-lucide-radio"
          :description="channelsLine(ace)"
          :section="aceCard"
          :loading="loading"
          @saved="saved"
        >
          <template #badges>
            <UBadge v-if="ace && !ace.configured" color="neutral" variant="subtle" size="sm" label="Not set up" />
          </template>

          <div class="flex flex-col gap-3">
            <div class="flex items-center gap-2">
              <h4 class="text-sm font-semibold">Engine</h4>
              <span class="text-xs text-dimmed">required</span>
            </div>
            <BaseUrlField
              :model-value="aceCard.draft.engine_url ?? ''"
              label="URL"
              help="lineup asks it for channel names and runs checks through it. Leave empty to turn Acestream off."
              kind="acestream"
              component="engine"
              :placeholder="examples.engine"
              :error="baseUrlError(aceCard.draft.engine_url ?? '', false)"
              @update:model-value="(v: string) => setDraft(aceCard, 'engine_url', v)"
            />
            <StatusLine :result="statusOf(status.ace.data, 'engine')" :error="status.ace.error" />
          </div>

          <USeparator />

          <div class="flex flex-col gap-3">
            <div class="flex items-center gap-2">
              <h4 class="text-sm font-semibold">acexy</h4>
              <span class="text-xs text-dimmed">optional, recommended</span>
            </div>
            <BaseUrlField
              :model-value="aceCard.draft.acexy_url ?? ''"
              label="URL"
              help="Players connect here, and several viewers share one engine session. Channel checks and live numbers need it."
              kind="acestream"
              component="acexy"
              :placeholder="examples.acexy"
              :error="baseUrlError(aceCard.draft.acexy_url ?? '', false)"
              @update:model-value="(v: string) => setDraft(aceCard, 'acexy_url', v)"
            />
            <StatusLine :result="statusOf(status.ace.data, 'acexy')" :error="status.ace.error" />
            <UAlert
              v-if="engineSet && !acexySet"
              color="info"
              variant="subtle"
              icon="i-lucide-info"
              title="Without acexy, playlists send players to the engine directly"
              description="That works for one viewer at a time. Channel checks are off: only acexy can tell lineup whether someone is watching a stream."
            />
          </div>

          <USeparator />

          <UFormField
            label="Check channels every (hours)"
            help="Checks each enabled acestream channel's swarm, skipping streams someone is watching. 0 turns the schedule off."
          >
            <UInput
              :model-value="aceCard.draft.probe_interval_hours"
              type="number"
              min="0"
              max="168"
              class="w-32"
              size="lg"
              @update:model-value="(v: unknown) => setDraft(aceCard, 'probe_interval_hours', Number(v))"
            />
          </UFormField>
          <p v-if="aceRules(aceCard.draft)" class="text-sm text-error">{{ aceRules(aceCard.draft) }}</p>

          <template #actions>
            <UButton
              icon="i-lucide-activity"
              color="neutral"
              variant="subtle"
              size="lg"
              class="min-h-11"
              label="Check all now"
              :loading="probing"
              :disabled="aceCard.dirty || loading || !ace?.checks.available"
              :title="
                aceCard.dirty ? 'Save first: checks use the saved config' : (ace?.checks.reason ?? undefined)
              "
              @click="checkAll"
            />
          </template>
        </SettingsCard>

        <!-- VK -->
        <SettingsCard
          title="VK proxy"
          icon="i-lucide-video"
          :description="channelsLine(vk)"
          :section="vkCard"
          :loading="loading"
          @saved="saved"
        >
          <template #badges>
            <UBadge v-if="vk && !vk.configured" color="neutral" variant="subtle" size="sm" label="Not set up" />
          </template>
          <BaseUrlField
            :model-value="vkCard.draft.url ?? ''"
            label="URL"
            help="VK channels play from {url}/vk/{owner}_{video}. Leave empty to turn VK off."
            kind="vk"
            component="proxy"
            :placeholder="examples.vk"
            :error="baseUrlError(vkCard.draft.url ?? '', false)"
            @update:model-value="(v: string) => setDraft(vkCard, 'url', v)"
          />
          <StatusLine :result="statusOf(status.vk.data, 'proxy')" :error="status.vk.error" />
        </SettingsCard>

        <!-- Direct -->
        <UCard :ui="{ body: 'p-4 sm:p-5' }">
          <div class="flex items-start gap-3">
            <UIcon name="i-lucide-link" class="size-5 mt-0.5 shrink-0 text-muted" />
            <div class="min-w-0 flex-1">
              <h3 class="font-semibold">Direct URLs</h3>
              <p class="text-sm text-muted">
                Any other http(s) stream URL, put in playlists as it is. Nothing to set up.
                <template v-if="direct"> {{ channelsLine(direct) }}</template>
              </p>
            </div>
          </div>
        </UCard>
      </div>
    </template>
  </UDashboardPanel>
</template>
