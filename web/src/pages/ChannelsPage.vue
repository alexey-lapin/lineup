<script setup lang="ts">
import { useToast } from '@nuxt/ui/composables'
import type { DropdownMenuItem, TableColumn } from '@nuxt/ui'
import { computed, ref } from 'vue'
import { api, sourceLabel, type Channel } from '../api'
import ChannelForm from '../components/ChannelForm.vue'
import { copy, playerLinks, useAction, useChannels, useInvalidateAll } from '../composables'

const toast = useToast()
const run = useAction()
const invalidate = useInvalidateAll()
const { data, isPending, error } = useChannels()

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

async function confirmDelete() {
  const c = deleting.value
  if (!c) return
  deleting.value = null
  if ((await run(() => api.deleteChannel(c.id), `Deleted ${c.name}`)).ok) await invalidate()
}

function actions(c: Channel): DropdownMenuItem[][] {
  const links = playerLinks(c.stream_url)
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
      { label: 'Copy stream URL', icon: 'i-lucide-clipboard', onSelect: () => copy(c.stream_url, toast) },
      { label: 'Open in VLC', icon: 'i-lucide-play', href: links.vlc },
      { label: 'Open in Infuse', icon: 'i-lucide-play', href: links.infuse }
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
          <UButton icon="i-lucide-plus" label="Add" @click="openAdd" />
        </template>
      </UDashboardNavbar>
      <UDashboardToolbar>
        <template #left>
          <UInput v-model="search" icon="i-lucide-search" placeholder="Search name, tvg-id, id…" class="w-full sm:w-72" />
        </template>
        <template #right>
          <USelect v-model="group" :items="groups" class="w-36" />
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
          <UDropdownMenu :items="actions(row.original)" :content="{ align: 'end' }">
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
