<script setup lang="ts">
import { useQuery } from '@pinia/colada'
import { ref, watch } from 'vue'
import { api } from '../api'
import { useAction, useInvalidateAll } from '../composables'

const run = useAction()
const invalidate = useInvalidateAll()
const { data: settings, error } = useQuery({ key: ['settings'], query: () => api.settings() })
const { data: status } = useQuery({ key: ['status'], query: () => api.status() })

const state = ref({
  acexy_base: '',
  engine_base: '',
  vk_base: '',
  epg_refresh_hours: 6
})
watch(
  settings,
  (s) => {
    if (!s) return
    state.value.acexy_base = s.acexy_base
    state.value.engine_base = s.engine_base
    state.value.vk_base = s.vk_base
    state.value.epg_refresh_hours = s.epg_refresh_hours
  },
  { immediate: true }
)

const saving = ref(false)

async function save() {
  saving.value = true
  const r = await run(
    () =>
      api.patchSettings({
        acexy_base: state.value.acexy_base,
        engine_base: state.value.engine_base,
        vk_base: state.value.vk_base,
        epg_refresh_hours: Number(state.value.epg_refresh_hours) || 0
      }),
    'Settings saved'
  )
  saving.value = false
  if (r.ok) await invalidate()
}
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
      <UAlert v-if="error" color="error" variant="subtle" :title="error.message" />
      <form class="flex flex-col gap-6 max-w-2xl" @submit.prevent="save">
        <section class="flex flex-col gap-4">
          <h2 class="font-semibold">Stream sources</h2>
          <p class="text-sm text-muted -mt-2">
            Playlists build stream URLs from these, so use addresses Jellyfin and your players can reach.
          </p>
          <UFormField label="acexy base URL" help="Acestream channels: {base}/ace/getstream?id=…">
            <UInput v-model="state.acexy_base" class="w-full" placeholder="http://media-host:8080" autocapitalize="off" />
          </UFormField>
          <UFormField label="VK proxy base URL" help="VK channels: {base}/vk/{owner}_{video}?list=…">
            <UInput v-model="state.vk_base" class="w-full" placeholder="http://media-host:8099" autocapitalize="off" />
          </UFormField>
          <UFormField label="Acestream engine URL" help="Used to look up channel names when adding, and for health checks">
            <UInput v-model="state.engine_base" class="w-full" placeholder="http://media-host:6878" autocapitalize="off" />
          </UFormField>
        </section>

        <section class="flex flex-col gap-4">
          <h2 class="font-semibold">Guide</h2>
          <UFormField
            label="Refresh every (hours)"
            help="Guide sources are fetched 3 minutes after start, then at this interval. 0 turns the schedule off."
          >
            <UInput v-model.number="state.epg_refresh_hours" type="number" min="0" max="168" class="w-32" />
          </UFormField>
        </section>

        <div class="flex gap-2">
          <UButton type="submit" label="Save" :loading="saving" />
        </div>
        <p v-if="status" class="text-xs text-dimmed">lineup {{ status.version }}</p>
      </form>
    </template>
  </UDashboardPanel>
</template>
