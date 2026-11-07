<script setup lang="ts">
import { useQuery } from '@pinia/colada'
import { useToast } from '@nuxt/ui/composables'
import type { DropdownMenuItem, TableColumn } from '@nuxt/ui'
import { computed, ref } from 'vue'
import { api, INTEGRATION_NAMES, sourceLabel, type Channel } from '../api'
import ChannelForm from '../components/ChannelForm.vue'
import { copy, playerLinks, useAction, useChannels, useInvalidateAll } from '../composables'
import { floating } from '../safe-area'

const toast = useToast()
const run = useAction()
const invalidate = useInvalidateAll()
const { data, isPending, error } = useChannels()
const { data: integrations } = useQuery({ key: ['integrations'], query: () => api.integrations() })
/** Whether acestream checks can run, and why not (unknown until the integrations load). */
const checks = computed(() => integrations.value?.find((i) => i.kind === 'acestream')?.checks)

function notSetUp(c: Channel) {
  return `The ${INTEGRATION_NAMES[c.source.kind]} integration is not set up: this channel is left out of playlists and guides.`
}

const search = ref('')
const group = ref('All')
const formOpen = ref(false)
const editing = ref<Channel | null>(null)
const deleting = ref<Channel | null>(null)

const groups = computed(() => [
  'All',
  ...[...new Set((data.value ?? []).map((c) => c.group_title ?? '(none)'))].sort()
])

const rows = computed(() => {
  const q = search.value.trim().toLowerCase()
  return (data.value ?? []).filter((c) => {
    const g = c.group_title ?? '(none)'
    if (group.value !== 'All' && g !== group.value) return false
    if (!q) return true
    return (
      c.name.toLowerCase().includes(q) ||
      (c.tvg_id ?? '').toLowerCase().includes(q) ||
      c.source_key.includes(q)
    )
  })
})

// Phones show name + actions; secondary columns appear on wider screens.
const hideSm = 'hidden sm:table-cell'
const hideMd = 'hidden md:table-cell'

const columns: TableColumn<Channel>[] = [
  { accessorKey: 'name', header: 'Name' },
  { accessorKey: 'group_title', header: 'Group', meta: { class: { th: hideSm, td: hideSm } } },
  { id: 'source', header: 'Source', meta: { class: { th: hideMd, td: hideMd } } },
  { id: 'playlists', header: 'Playlists', meta: { class: { th: hideMd, td: hideMd } } },
  { id: 'actions', meta: { class: { td: 'text-right' } } }
]

function openAdd() {
  editing.value = null
  formOpen.value = true
}
function openEdit(c: Channel) {
  editing.value = c
  formOpen.value = true
}

async function toggle(c: Channel) {
  if ((await run(() => api.patchChannel(c.id, { enabled: !c.enabled }), c.enabled ? 'Disabled' : 'Enabled')).ok) {
    await invalidate()
  }
}

const probing = ref<number | 'all' | null>(null)

async function probe(c: Channel) {
  probing.value = c.id
  const r = await run(() => api.probe([c.id]))
  probing.value = null
  if (!r.ok) return
  await invalidate()
  const res = r.value.results[0]
  if (!res) return
  const reason = (res.detail as { error?: string; reason?: string } | null)?.error ?? (res.detail as { reason?: string } | null)?.reason
  const color = res.outcome === 'ok' ? 'success' : res.outcome === 'dead' ? 'error' : 'warning'
  toast.add({ title: `${c.name}: ${res.outcome}`, description: reason, color })
}

async function probeAll() {
  probing.value = 'all'
  const r = await run(() => api.probe([]), 'Probing all acestream channels in the background')
  probing.value = null
  void r
}

/** Badge for the last conclusive probe; a hint when the latest attempt was inconclusive. */
function health(c: Channel) {
  const h = c.health
  if (!h) return null
  const color = h.conclusive === 'ok' ? 'success' : h.conclusive === 'dead' ? 'error' : 'neutral'
  const label = h.conclusive ?? h.last_attempt
  const hint =
    h.conclusive !== h.last_attempt
      ? `Latest attempt ${new Date(h.last_attempt_at).toLocaleString()}: ${h.last_attempt}`
      : `Checked ${new Date(h.last_attempt_at).toLocaleString()}`
  return { color, label, hint, stale: h.conclusive !== h.last_attempt } as const
}

async function confirmDelete() {
  const c = deleting.value
  if (!c) return
  deleting.value = null
  if ((await run(() => api.deleteChannel(c.id), `Deleted ${c.name}`)).ok) await invalidate()
}

function actions(c: Channel): DropdownMenuItem[][] {
  const url = c.stream_url
  const links = url ? playerLinks(url) : null
  return [
    [
      { label: 'Edit', icon: 'i-lucide-pencil', onSelect: () => openEdit(c) },
      {
        label: c.enabled ? 'Disable' : 'Enable',
        icon: c.enabled ? 'i-lucide-eye-off' : 'i-lucide-eye',
        onSelect: () => toggle(c)
      }
    ],
    [
      ...(c.source.kind === 'acestream'
        ? [
            {
              label: 'Check health',
              icon: 'i-lucide-activity',
              disabled: checks.value?.available === false,
              onSelect: () => probe(c)
            }
          ]
        : []),
      ...(url && links
        ? [
            { label: 'Copy stream URL', icon: 'i-lucide-clipboard', onSelect: () => copy(url, toast) },
            { label: 'Open in VLC', icon: 'i-lucide-play', href: links.vlc },
            { label: 'Open in Infuse', icon: 'i-lucide-play', href: links.infuse }
          ]
        : [])
    ],
    [{ label: 'Delete', icon: 'i-lucide-trash', color: 'error', onSelect: () => (deleting.value = c) }]
  ]
}
</script>

<template>
  <UDashboardPanel id="channels">
    <template #header>
      <UDashboardNavbar title="Channels">
        <template #leading>
          <UDashboardSidebarCollapse />
        </template>
        <template #right>
          <UButton
            icon="i-lucide-activity"
            color="neutral"
            variant="subtle"
            label="Check all"
            class="hidden sm:inline-flex"
            :loading="probing === 'all'"
            :disabled="checks?.available === false"
            :title="checks?.reason ?? undefined"
            @click="probeAll"
          />
          <UButton icon="i-lucide-plus" label="Add" @click="openAdd" />
        </template>
      </UDashboardNavbar>
      <UDashboardToolbar>
        <template #left>
          <UInput v-model="search" icon="i-lucide-search" placeholder="Search name, tvg-id, id…" class="w-full sm:w-72" />
        </template>
        <template #right>
          <USelect v-model="group" :content="floating()" :items="groups" class="w-36" />
        </template>
      </UDashboardToolbar>
    </template>

    <template #body>
      <UAlert v-if="error" color="error" variant="subtle" icon="i-lucide-circle-x" :title="error.message" />
      <UTable :data="rows" :columns="columns" :loading="isPending" class="flex-1" :ui="{ td: 'py-2' }">
        <template #name-cell="{ row }">
          <div class="flex items-center gap-2 min-w-0">
            <img
              v-if="row.original.tvg_logo"
              :src="row.original.tvg_logo"
              alt=""
              class="size-6 object-contain shrink-0"
              loading="lazy"
            />
            <UIcon v-else name="i-lucide-tv" class="size-6 text-dimmed shrink-0" />
            <div class="min-w-0">
              <div class="font-medium truncate" :class="{ 'text-muted line-through': !row.original.enabled }">
                {{ row.original.name }}
              </div>
              <div v-if="row.original.tvg_id" class="text-xs text-muted truncate">{{ row.original.tvg_id }}</div>
            </div>
            <UBadge v-if="row.original.is_event" label="event" color="warning" variant="subtle" size="sm" />
            <UTooltip v-if="!row.original.configured" :content="floating()" :text="notSetUp(row.original)">
              <UBadge label="not set up" color="neutral" variant="outline" size="sm" icon="i-lucide-plug" />
            </UTooltip>
            <UTooltip v-if="health(row.original)" :content="floating()" :text="health(row.original)!.hint">
              <UBadge
                :label="health(row.original)!.label"
                :color="health(row.original)!.color"
                :variant="health(row.original)!.stale ? 'outline' : 'subtle'"
                size="sm"
              />
            </UTooltip>
            <UIcon v-if="probing === row.original.id" name="i-lucide-loader-circle" class="size-4 animate-spin text-muted" />
          </div>
        </template>
        <template #group_title-cell="{ row }">
          <span class="text-muted">{{ row.original.group_title ?? '—' }}</span>
        </template>
        <template #source-cell="{ row }">
          <UBadge
            :label="sourceLabel(row.original.source)"
            color="neutral"
            variant="outline"
            class="font-mono max-w-48 truncate"
          />
        </template>
        <template #playlists-cell="{ row }">
          <div class="flex flex-wrap gap-1">
            <UBadge v-for="p in row.original.playlists" :key="p" :label="p" variant="subtle" size="sm" />
          </div>
        </template>
        <template #actions-cell="{ row }">
          <UDropdownMenu :items="actions(row.original)" :content="floating({ align: 'end' as const })">
            <UButton icon="i-lucide-ellipsis-vertical" color="neutral" variant="ghost" aria-label="Actions" />
          </UDropdownMenu>
        </template>
        <template #empty>
          <div class="py-10 text-center text-muted">
            No channels yet. Add one, or import your existing m3u.
          </div>
        </template>
      </UTable>

      <ChannelForm v-model:open="formOpen" :channel="editing" />

      <UModal
        :open="!!deleting"
        :title="`Delete ${deleting?.name}?`"
        description="It is removed from every playlist; the remaining channels are renumbered."
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
