<script setup lang="ts">
import { computed, ref } from 'vue'
import { api, type Route } from '../api'
import { liveEpoch, useAction, useInvalidateAll, usePolling } from '../composables'

/**
 * One route: what it is, its gateway's live status (from the saved config), the last resync, and
 * its actions. Editing and deleting are asked of the page.
 */
const props = defineProps<{ route: Route }>()
const emit = defineEmits<{ edit: [Route]; delete: [Route] }>()

const run = useAction()
const invalidate = useInvalidateAll()

const gateway = computed(() => (props.route.config.kind === 'gateway' ? props.route.config : null))

/** Live status of a gateway route; a change to the route (new revision) polls again at once. */
const status = usePolling(
  () => (gateway.value ? `${props.route.id}:${props.route.revision}:${liveEpoch.value}` : undefined),
  (_, signal) => api.routeStatus(props.route.id, signal),
  30000
)

const channelsLine = computed(() =>
  props.route.channel_count === 1 ? 'Used by 1 channel.' : `Used by ${props.route.channel_count} channels.`
)

const busy = ref<'resync' | 'toggle' | null>(null)
async function resync() {
  busy.value = 'resync'
  const r = await run(() => api.resyncRoute(props.route.id))
  busy.value = null
  if (!r.ok) return
  await invalidate()
  void status.refresh()
}
async function toggle() {
  busy.value = 'toggle'
  const on = !props.route.enabled
  const r = await run(() => api.patchRoute(props.route.id, { enabled: on }), on ? 'Route enabled' : 'Route disabled')
  busy.value = null
  if (r.ok) await invalidate()
}

const when = (t: string) => new Date(t).toLocaleString()
const showFailures = ref(false)
const problems = computed(() => [...(props.route.last_resync?.failed ?? []), ...(props.route.last_resync?.skipped ?? [])])
</script>

<template>
  <UCard :ui="{ body: 'p-4 sm:p-5 flex flex-col gap-3' }">
    <div class="flex items-start gap-3">
      <UIcon :name="gateway ? 'i-lucide-route' : 'i-lucide-link'" class="size-5 mt-0.5 shrink-0 text-muted" />
      <div class="min-w-0 flex-1">
        <div class="flex items-center gap-2 flex-wrap">
          <h3 class="font-semibold">{{ route.name }}</h3>
          <UBadge :label="gateway ? 'gateway' : 'direct'" color="neutral" variant="subtle" size="sm" />
          <UBadge v-if="!route.enabled" label="disabled" color="warning" variant="subtle" size="sm" />
        </div>
        <p class="text-sm text-muted">
          <template v-if="gateway">
            Rule <span class="font-mono">{{ gateway.rule }}</span> on
            <span class="font-mono break-all">{{ gateway.base_url }}</span>.
          </template>
          <template v-else>Stream URLs, played as they are.</template>
          {{ channelsLine }}
          <template v-if="!route.enabled"> Its channels are left out of playlists and guides.</template>
        </p>
      </div>
    </div>

    <!-- Live status of the gateway -->
    <p v-if="gateway" role="status" aria-live="polite" class="text-sm flex items-start gap-1.5">
      <template v-if="status.data.value && !status.error.value">
        <UIcon
          :name="status.data.value.ok ? 'i-lucide-circle-check' : 'i-lucide-circle-alert'"
          class="size-4 mt-0.5 shrink-0"
          :class="status.data.value.ok ? 'text-success' : 'text-error'"
        />
        <span>
          {{ status.data.value.detail }}
          <template v-if="status.data.value.rule">
            · {{ status.data.value.rule.title }}
            <template v-if="status.data.value.rule.capabilities.length">
              ({{ status.data.value.rule.capabilities.join(', ') }})
            </template>
          </template>
          <template v-if="status.data.value.pending_channels">
            · {{ status.data.value.pending_channels }} channel(s) pending a resync
          </template>
        </span>
      </template>
      <template v-else-if="status.error.value">
        <UIcon name="i-lucide-circle-help" class="size-4 mt-0.5 shrink-0 text-muted" />
        <span class="text-muted">Status unavailable: {{ status.error.value }}</span>
      </template>
      <span v-else class="text-muted">Asking the gateway…</span>
    </p>

    <!-- Last resync -->
    <div v-if="route.last_resync" class="text-sm text-muted">
      Last resync {{ when(route.last_resync.at) }}: {{ route.last_resync.updated }} updated,
      {{ route.last_resync.unchanged }} unchanged<template v-if="problems.length">,
        <button type="button" class="underline" @click="showFailures = !showFailures">
          {{ problems.length }} not updated
        </button></template
      >.
      <ul v-if="showFailures" class="mt-1 list-disc pl-5">
        <li v-for="p in problems" :key="p.channel_id">
          <ULink :to="`/channels/${p.channel_id}`" class="underline">{{ p.name }}</ULink>: {{ p.reason }}
        </li>
      </ul>
    </div>

    <div class="flex flex-wrap gap-2">
      <UButton
        v-if="gateway"
        icon="i-lucide-refresh-cw"
        color="neutral"
        variant="subtle"
        label="Resync"
        title="Revalidate every channel with the gateway now"
        :loading="busy === 'resync'"
        @click="resync"
      />
      <UButton
        :icon="route.enabled ? 'i-lucide-eye-off' : 'i-lucide-eye'"
        color="neutral"
        variant="subtle"
        :label="route.enabled ? 'Disable' : 'Enable'"
        :loading="busy === 'toggle'"
        @click="toggle"
      />
      <UButton icon="i-lucide-pencil" color="neutral" variant="subtle" label="Edit" @click="emit('edit', route)" />
      <UButton
        icon="i-lucide-trash"
        color="error"
        variant="ghost"
        label="Delete"
        :disabled="route.channel_count > 0"
        :title="route.channel_count > 0 ? 'Move or delete its channels first' : undefined"
        @click="emit('delete', route)"
      />
    </div>
  </UCard>
</template>
