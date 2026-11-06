<script setup lang="ts">
import { useQuery, useQueryCache } from '@pinia/colada'
import { useToast } from '@nuxt/ui/composables'
import { computed, ref } from 'vue'
import { api, EVENT_TYPES, SECRET_MASK, type Hook, type HookConfig, type HookDelivery } from '../api'
import { useAction } from '../composables'
import { floating } from '../safe-area'

const run = useAction()
const toast = useToast()
const cache = useQueryCache()
const { data: hooks, error, isPending } = useQuery({ key: ['hooks'], query: () => api.hooks() })
const refresh = () => cache.invalidateQueries({ key: ['hooks'] })

// ---------------------------------------------------------------- form

type Kind = 'jellyfin' | 'webhook'
const formOpen = ref(false)
const editing = ref<Hook | null>(null)
const saving = ref(false)
const form = ref({
  kind: 'jellyfin' as Kind,
  name: '',
  base_url: '',
  api_key: '',
  task_id: '',
  followup_minutes: 65,
  method: 'POST',
  url: '',
  headers: [] as { name: string; value: string }[],
  events: ['playlist.changed', 'epg.updated'] as string[],
  debounce_secs: 30
})

const kindItems = [
  { label: 'Jellyfin guide refresh', value: 'jellyfin' },
  { label: 'Webhook', value: 'webhook' }
]
const eventItems = EVENT_TYPES.map((e) => ({ label: e as string, value: e as string }))

function openAdd() {
  editing.value = null
  form.value = {
    kind: 'jellyfin',
    name: 'Jellyfin',
    base_url: '',
    api_key: '',
    task_id: '',
    followup_minutes: 65,
    method: 'POST',
    url: '',
    headers: [],
    events: ['playlist.changed', 'epg.updated'],
    debounce_secs: 30
  }
  formOpen.value = true
}

function openEdit(h: Hook) {
  editing.value = h
  const c = h.config
  form.value = {
    kind: c.kind,
    name: h.name,
    base_url: c.kind === 'jellyfin' ? c.base_url : '',
    api_key: c.kind === 'jellyfin' ? c.api_key : '',
    task_id: c.kind === 'jellyfin' ? (c.task_id ?? '') : '',
    followup_minutes: c.kind === 'jellyfin' ? (c.followup_minutes ?? 65) : 65,
    method: c.kind === 'webhook' ? (c.method ?? 'POST') : 'POST',
    url: c.kind === 'webhook' ? c.url : '',
    headers: c.kind === 'webhook' ? Object.entries(c.headers ?? {}).map(([name, value]) => ({ name, value })) : [],
    events: [...h.events],
    debounce_secs: h.debounce_secs
  }
  formOpen.value = true
}

function onKindChange(k: Kind) {
  if (editing.value) return
  form.value.events = k === 'jellyfin' ? ['playlist.changed', 'epg.updated'] : [...EVENT_TYPES]
  if (k === 'webhook' && form.value.name === 'Jellyfin') form.value.name = 'Webhook'
  if (k === 'jellyfin' && form.value.name === 'Webhook') form.value.name = 'Jellyfin'
}

function config(): HookConfig {
  const f = form.value
  if (f.kind === 'jellyfin') {
    return {
      kind: 'jellyfin',
      base_url: f.base_url.trim(),
      api_key: f.api_key.trim(),
      task_id: f.task_id.trim() || null,
      followup_minutes: Number(f.followup_minutes) || 0
    }
  }
  return {
    kind: 'webhook',
    method: f.method,
    url: f.url.trim(),
    headers: Object.fromEntries(f.headers.filter((h) => h.name.trim()).map((h) => [h.name.trim(), h.value]))
  }
}

const canSave = computed(() => {
  const f = form.value
  if (!f.name.trim() || !f.events.length || saving.value) return false
  return f.kind === 'jellyfin' ? !!f.base_url.trim() && !!f.api_key.trim() : !!f.url.trim()
})

async function save() {
  if (!canSave.value) return
  saving.value = true
  const e = editing.value
  const body = {
    name: form.value.name.trim(),
    config: config(),
    events: form.value.events,
    debounce_secs: Number(form.value.debounce_secs) || 0
  }
  const r = e
    ? await run(() => api.patchHook(e.id, body), 'Hook saved')
    : await run(() => api.createHook({ ...body, enabled: true }), 'Hook added')
  saving.value = false
  if (r.ok) {
    formOpen.value = false
    await refresh()
  }
}

// ---------------------------------------------------------------- actions

async function toggle(h: Hook) {
  if ((await run(() => api.patchHook(h.id, { enabled: !h.enabled }), h.enabled ? 'Disabled' : 'Enabled')).ok) {
    await refresh()
  }
}

const testing = ref<number | null>(null)
async function test(h: Hook) {
  testing.value = h.id
  const r = await run(() => api.testHook(h.id))
  testing.value = null
  if (!r.ok) return
  const d = r.value
  toast.add(
    d.ok
      ? { title: 'Test delivered', description: `HTTP ${d.http_code}`, color: 'success', icon: 'i-lucide-check' }
      : { title: 'Test failed', description: d.error ?? undefined, color: 'error', icon: 'i-lucide-circle-x' }
  )
}

const deleting = ref<Hook | null>(null)
async function confirmDelete() {
  const h = deleting.value
  if (!h) return
  deleting.value = null
  if ((await run(() => api.deleteHook(h.id), `Deleted ${h.name}`)).ok) await refresh()
}

// ---------------------------------------------------------------- delivery log

const logFor = ref<Hook | null>(null)
const log = ref<HookDelivery[]>([])
const logLoading = ref(false)
async function loadLog(h: Hook) {
  logFor.value = h
  logLoading.value = true
  const r = await run(() => api.hookDeliveries(h.id, 50))
  logLoading.value = false
  log.value = r.ok ? r.value : []
}

function target(h: Hook) {
  return h.config.kind === 'jellyfin' ? h.config.base_url : `${h.config.method ?? 'POST'} ${h.config.url}`
}
function triggerLabel(d: HookDelivery) {
  return d.trigger === 'followup' ? 'follow-up' : d.trigger
}
</script>

<template>
  <UDashboardPanel id="hooks">
    <template #header>
      <UDashboardNavbar title="Hooks">
        <template #leading>
          <UDashboardSidebarCollapse />
        </template>
        <template #right>
          <UButton icon="i-lucide-plus" label="Add hook" @click="openAdd" />
        </template>
      </UDashboardNavbar>
    </template>

    <template #body>
      <UAlert v-if="error" color="error" variant="subtle" :title="error.message" />
      <p v-if="!isPending && !hooks?.length" class="text-sm text-muted max-w-2xl">
        Hooks run when playlists or guide data change. A Jellyfin hook starts its "Refresh Guide" task, so
        Jellyfin picks up changes without waiting for its schedule. A webhook sends the events as JSON to anything else.
      </p>

      <div class="flex flex-col gap-3">
        <UCard v-for="h in hooks" :key="h.id" :ui="{ body: 'p-4 sm:p-4' }">
          <div class="flex items-start justify-between gap-3">
            <div class="min-w-0 flex flex-col gap-1">
              <div class="flex items-center gap-2 min-w-0">
                <UIcon
                  :name="h.config.kind === 'jellyfin' ? 'i-lucide-tv-minimal-play' : 'i-lucide-webhook'"
                  class="size-4 shrink-0 text-muted"
                />
                <span class="font-semibold truncate" :class="{ 'text-muted': !h.enabled }">{{ h.name }}</span>
                <UBadge v-if="!h.enabled" color="neutral" variant="subtle" size="sm" label="disabled" />
              </div>
              <div class="text-xs text-muted break-all">{{ target(h) }}</div>
              <div class="flex flex-wrap gap-1">
                <UBadge v-for="e in h.events" :key="e" color="neutral" variant="outline" size="sm" :label="e" />
              </div>
              <div class="text-xs text-dimmed">
                debounce {{ h.debounce_secs }}s
                <template v-if="h.config.kind === 'jellyfin' && (h.config.followup_minutes ?? 0) > 0">
                  · follow-up after {{ h.config.followup_minutes }} min
                </template>
              </div>
            </div>
            <div class="flex items-center gap-1 shrink-0">
              <USwitch :model-value="h.enabled" aria-label="Enabled" @update:model-value="toggle(h)" />
              <UButton
                icon="i-lucide-send"
                color="neutral"
                variant="ghost"
                aria-label="Send a test"
                :loading="testing === h.id"
                @click="test(h)"
              />
              <UDropdownMenu
                :content="floating()"
                :items="[
                  [
                    { label: 'Edit', icon: 'i-lucide-pencil', onSelect: () => openEdit(h) },
                    { label: 'Delivery log', icon: 'i-lucide-scroll-text', onSelect: () => loadLog(h) }
                  ],
                  [{ label: 'Delete', icon: 'i-lucide-trash-2', color: 'error', onSelect: () => (deleting = h) }]
                ]"
              >
                <UButton icon="i-lucide-ellipsis-vertical" color="neutral" variant="ghost" aria-label="More" />
              </UDropdownMenu>
            </div>
          </div>
        </UCard>
      </div>

      <USlideover v-model:open="formOpen" :title="editing ? `Edit ${editing.name}` : 'Add hook'">
        <template #body>
          <form class="flex flex-col gap-4" @submit.prevent="save">
            <UFormField label="Kind">
              <USelect
                v-model="form.kind"
                :content="floating()"
                :items="kindItems"
                class="w-full"
                :disabled="!!editing"
                @update:model-value="(k) => onKindChange(k as Kind)"
              />
            </UFormField>
            <UFormField label="Name" required>
              <UInput v-model="form.name" class="w-full" />
            </UFormField>

            <template v-if="form.kind === 'jellyfin'">
              <UFormField label="Jellyfin URL" required>
                <UInput v-model="form.base_url" class="w-full" placeholder="http://jellyfin:8096" autocapitalize="off" />
              </UFormField>
              <UFormField
                label="API key"
                required
                :help="form.api_key === SECRET_MASK ? 'Stored; type a new key to replace it' : 'Dashboard → API Keys'"
              >
                <UInput v-model="form.api_key" type="password" class="w-full" autocomplete="off" />
              </UFormField>
              <UFormField label="Task id" help="Leave empty to find the Refresh Guide task automatically">
                <UInput v-model="form.task_id" class="w-full" autocapitalize="off" />
              </UFormField>
              <UFormField
                label="Follow-up refresh (minutes)"
                help="Jellyfin caches a downloaded guide for about an hour; a second refresh after that picks up guide changes. 0 turns it off."
              >
                <UInput v-model.number="form.followup_minutes" type="number" min="0" class="w-32" />
              </UFormField>
            </template>

            <template v-else>
              <UFormField label="URL" required>
                <UInput v-model="form.url" class="w-full" placeholder="https://example.org/hook" autocapitalize="off" />
              </UFormField>
              <UFormField label="Method">
                <USelect v-model="form.method" :content="floating()" :items="['POST', 'PUT', 'PATCH']" class="w-32" />
              </UFormField>
              <UFormField label="Headers" help="Values are stored write-only">
                <div class="flex flex-col gap-2">
                  <div v-for="(h, i) in form.headers" :key="i" class="flex gap-2">
                    <UInput v-model="h.name" placeholder="Name" class="w-2/5" autocapitalize="off" />
                    <UInput v-model="h.value" placeholder="Value" type="password" class="flex-1" autocomplete="off" />
                    <UButton
                      icon="i-lucide-x"
                      color="neutral"
                      variant="ghost"
                      aria-label="Remove header"
                      @click="form.headers.splice(i, 1)"
                    />
                  </div>
                  <UButton
                    icon="i-lucide-plus"
                    color="neutral"
                    variant="subtle"
                    size="sm"
                    label="Add header"
                    class="self-start"
                    @click="form.headers.push({ name: '', value: '' })"
                  />
                </div>
              </UFormField>
            </template>

            <UFormField label="Events">
              <USelectMenu v-model="form.events" :content="floating()" :items="eventItems" value-key="value" multiple class="w-full" />
            </UFormField>
            <UFormField label="Debounce (seconds)" help="Changes are collected until none has arrived for this long">
              <UInput v-model.number="form.debounce_secs" type="number" min="0" class="w-32" />
            </UFormField>
            <button type="submit" class="hidden" />
          </form>
        </template>
        <template #footer>
          <div class="flex justify-end gap-2 w-full">
            <UButton color="neutral" variant="ghost" label="Cancel" @click="formOpen = false" />
            <UButton :label="editing ? 'Save' : 'Add'" :loading="saving" :disabled="!canSave" @click="save" />
          </div>
        </template>
      </USlideover>

      <UModal
        :open="!!logFor"
        :title="`Deliveries: ${logFor?.name}`"
        :ui="{ content: 'sm:max-w-2xl' }"
        @update:open="(o: boolean) => !o && (logFor = null)"
      >
        <template #body>
          <p v-if="!logLoading && !log.length" class="text-sm text-muted">Nothing delivered yet.</p>
          <ul v-else class="divide-y divide-default">
            <li v-for="d in log" :key="d.id" class="py-2 flex items-start gap-3 text-sm">
              <UIcon
                :name="d.ok ? 'i-lucide-circle-check' : 'i-lucide-circle-x'"
                :class="d.ok ? 'text-success' : 'text-error'"
                class="size-4 mt-0.5 shrink-0"
              />
              <div class="min-w-0 flex-1">
                <div>
                  {{ new Date(d.at).toLocaleString() }} · {{ triggerLabel(d) }}
                  <span v-if="d.attempt > 1" class="text-muted">(try {{ d.attempt }})</span>
                </div>
                <div class="text-muted break-all">
                  <template v-if="d.ok">HTTP {{ d.http_code }}</template>
                  <template v-else>{{ d.error }}</template>
                  <template v-if="d.events.length"> · {{ d.events.join(', ') }}</template>
                </div>
              </div>
            </li>
          </ul>
        </template>
      </UModal>

      <UModal
        :open="!!deleting"
        :title="`Delete ${deleting?.name}?`"
        description="Its delivery log is removed too."
        @update:open="(o: boolean) => !o && (deleting = null)"
      >
        <template #footer>
          <div class="flex justify-end gap-2 w-full">
            <UButton color="neutral" variant="ghost" label="Cancel" @click="deleting = null" />
            <UButton color="error" label="Delete" @click="confirmDelete" />
          </div>
        </template>
      </UModal>
    </template>
  </UDashboardPanel>
</template>
