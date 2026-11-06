<script setup lang="ts">
import { useQuery, useQueryCache } from '@pinia/colada'
import { useToast } from '@nuxt/ui/composables'
import { onBeforeUnmount, ref } from 'vue'
import { api, type EpgFetchResult, type EpgSource } from '../api'
import EpgSearch from '../components/EpgSearch.vue'
import { copy, useAction } from '../composables'
import { floating } from '../safe-area'

const toast = useToast()
const run = useAction()
const cache = useQueryCache()
const { data: sources, error, isPending } = useQuery({ key: ['epg-sources'], query: () => api.epgSources() })
const refreshSources = () => cache.invalidateQueries({ key: ['epg-sources'] })

// ---------------------------------------------------------------- polling after background fetches

/** Adding a source or changing its URL starts a fetch on the server; poll until it reports. */
let pollTimer: ReturnType<typeof setTimeout> | undefined
function pollUntilFetched(id: number, since: string | null, tries = 40) {
  clearTimeout(pollTimer)
  pollTimer = setTimeout(async () => {
    await refreshSources()
    const s = sources.value?.find((x) => x.id === id)
    if (s && s.last_fetch_at !== since) return
    if (tries > 1) pollUntilFetched(id, since, tries - 1)
  }, 3000)
}
onBeforeUnmount(() => clearTimeout(pollTimer))

// ---------------------------------------------------------------- fetch now

const fetching = ref<number | 'all' | null>(null)

function summarize(results: EpgFetchResult[]) {
  const failed = results.filter((r) => r.status === 'error')
  if (failed.length) {
    toast.add({
      title: `${failed.length} source${failed.length > 1 ? 's' : ''} failed`,
      description: failed.map((r) => `#${r.source_id}: ${r.error}`).join('\n'),
      color: 'error',
      icon: 'i-lucide-circle-x'
    })
    return
  }
  const updated = results.filter((r) => r.status === 'updated').length
  toast.add({
    title: updated ? `Updated ${updated} source${updated > 1 ? 's' : ''}` : 'Already up to date',
    color: 'success',
    icon: 'i-lucide-check'
  })
}

async function fetchNow(id?: number) {
  fetching.value = id ?? 'all'
  const r = await run(() => api.fetchEpg(id ? [id] : []))
  fetching.value = null
  await refreshSources()
  if (r.ok) summarize(r.value)
}

// ---------------------------------------------------------------- add / edit / delete

const formOpen = ref(false)
const editing = ref<EpgSource | null>(null)
const form = ref({ url: '', name: '', priority: 0 })
const saving = ref(false)

function openAdd() {
  editing.value = null
  form.value = { url: '', name: '', priority: 0 }
  formOpen.value = true
}
function openEdit(s: EpgSource) {
  editing.value = s
  form.value = { url: s.url, name: s.name, priority: s.priority }
  formOpen.value = true
}

async function save() {
  saving.value = true
  const e = editing.value
  const r = e
    ? await run(
        () =>
          api.patchEpgSource(e.id, {
            url: form.value.url.trim(),
            name: form.value.name.trim() || undefined,
            priority: Number(form.value.priority) || 0
          }),
        'Source saved'
      )
    : await run(
        () =>
          api.createEpgSource({
            url: form.value.url.trim(),
            name: form.value.name.trim() || null,
            priority: Number(form.value.priority) || 0,
            enabled: true
          }),
        'Source added, fetching now'
      )
  saving.value = false
  if (!r.ok) return
  formOpen.value = false
  await refreshSources()
  const urlChanged = !e || e.url !== r.value.url
  if (urlChanged) pollUntilFetched(r.value.id, r.value.last_fetch_at ?? null)
}

async function toggle(s: EpgSource) {
  const r = await run(() => api.patchEpgSource(s.id, { enabled: !s.enabled }), s.enabled ? 'Disabled' : 'Enabled')
  if (!r.ok) return
  await refreshSources()
  if (r.value.enabled && r.value.current_gen == null) pollUntilFetched(s.id, r.value.last_fetch_at ?? null)
}

const deleting = ref<EpgSource | null>(null)
async function confirmDelete() {
  const s = deleting.value
  if (!s) return
  deleting.value = null
  if ((await run(() => api.deleteEpgSource(s.id), `Deleted ${s.name}`)).ok) await refreshSources()
}

function statusColor(s: EpgSource) {
  switch (s.last_status) {
    case 'ok':
    case 'not_modified':
      return 'success' as const
    case 'error':
      return 'error' as const
    default:
      return 'neutral' as const
  }
}
function statusLabel(s: EpgSource) {
  if (!s.enabled) return 'disabled'
  switch (s.last_status) {
    case 'ok':
      return 'ok'
    case 'not_modified':
      return 'up to date'
    case 'error':
      return 'error'
    default:
      return 'not fetched yet'
  }
}
function when(iso?: string | null) {
  return iso ? new Date(iso).toLocaleString() : 'never'
}

</script>

<template>
  <UDashboardPanel id="epg">
    <template #header>
      <UDashboardNavbar title="EPG">
        <template #leading>
          <UDashboardSidebarCollapse />
        </template>
        <template #right>
          <UButton
            icon="i-lucide-refresh-cw"
            color="neutral"
            variant="subtle"
            label="Fetch all"
            :loading="fetching === 'all'"
            :disabled="fetching !== null || !sources?.length"
            @click="fetchNow()"
          />
          <UButton icon="i-lucide-plus" label="Add source" @click="openAdd" />
        </template>
      </UDashboardNavbar>
    </template>

    <template #body>
      <UAlert v-if="error" color="error" variant="subtle" :title="error.message" />

      <section class="flex flex-col gap-3">
        <h2 class="font-semibold">Sources</h2>
        <p v-if="!isPending && !sources?.length" class="text-sm text-muted">
          No guide sources yet. Add an XMLTV URL (plain or .gz); each playlist then serves its own guide at
          <code>/p/&lt;slug&gt;/epg.xml</code>.
        </p>
        <UCard v-for="s in sources" :key="s.id" :ui="{ body: 'p-4 sm:p-4' }">
          <div class="flex items-start justify-between gap-3">
            <div class="min-w-0 flex flex-col gap-1">
              <div class="flex items-center gap-2 min-w-0">
                <span class="font-semibold truncate" :class="{ 'text-muted': !s.enabled }">{{ s.name }}</span>
                <UBadge :color="statusColor(s)" variant="subtle" size="sm" :label="statusLabel(s)" />
              </div>
              <div class="text-xs text-muted break-all">{{ s.url }}</div>
              <div class="text-sm text-muted">
                {{ s.channel_count }} channels · priority {{ s.priority }} · last fetch {{ when(s.last_fetch_at) }}
              </div>
              <div v-if="s.last_status === 'error' && s.last_error" class="text-sm text-error break-all">
                {{ s.last_error }}
              </div>
            </div>
            <div class="flex items-center gap-1 shrink-0">
              <USwitch :model-value="s.enabled" aria-label="Enabled" @update:model-value="toggle(s)" />
              <UButton
                icon="i-lucide-refresh-cw"
                color="neutral"
                variant="ghost"
                aria-label="Fetch now"
                :loading="fetching === s.id"
                :disabled="fetching !== null || !s.enabled"
                @click="fetchNow(s.id)"
              />
              <UDropdownMenu
                :content="floating()"
                :items="[
                  [{ label: 'Edit', icon: 'i-lucide-pencil', onSelect: () => openEdit(s) }],
                  [{ label: 'Delete', icon: 'i-lucide-trash-2', color: 'error', onSelect: () => (deleting = s) }]
                ]"
              >
                <UButton icon="i-lucide-ellipsis-vertical" color="neutral" variant="ghost" aria-label="More" />
              </UDropdownMenu>
            </div>
          </div>
        </UCard>
      </section>

      <section class="flex flex-col gap-3 mt-6">
        <h2 class="font-semibold">Find a channel</h2>
        <EpgSearch
          class="sm:max-w-xl"
          action-label="Copy id"
          action-icon="i-lucide-copy"
          @select="(c) => copy(c.xmltv_id, toast)"
        />
      </section>

      <UModal v-model:open="formOpen" :title="editing ? `Edit ${editing.name}` : 'Add guide source'">
        <template #body>
          <form class="flex flex-col gap-4" @submit.prevent="save">
            <UFormField label="XMLTV URL" help="Plain or gzipped" required>
              <UInput
                v-model="form.url"
                class="w-full"
                placeholder="https://example.org/guide.xml.gz"
                autocapitalize="off"
                autocomplete="off"
              />
            </UFormField>
            <UFormField label="Name" :help="editing ? undefined : 'Defaults to the URL\'s host and file'">
              <UInput v-model="form.name" class="w-full" />
            </UFormField>
            <UFormField label="Priority" help="When sources share a channel id, the higher priority wins">
              <UInput v-model.number="form.priority" type="number" class="w-32" />
            </UFormField>
            <button type="submit" class="hidden" />
          </form>
        </template>
        <template #footer>
          <div class="flex justify-end gap-2 w-full">
            <UButton color="neutral" variant="ghost" label="Cancel" @click="formOpen = false" />
            <UButton
              :label="editing ? 'Save' : 'Add'"
              :loading="saving"
              :disabled="!form.url.trim() || saving"
              @click="save"
            />
          </div>
        </template>
      </UModal>

      <UModal
        :open="!!deleting"
        :title="`Delete ${deleting?.name}?`"
        description="Its downloaded guide data is removed too. Channels keep their tvg-ids."
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
