<script setup lang="ts">
import { useToast } from '@nuxt/ui/composables'
import { useQuery } from '@pinia/colada'
import { computed, reactive, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { VueDraggable } from 'vue-draggable-plus'
import { api, guideUrl, playlistUrl, sourceLabel, type PlaylistDetail } from '../api'
import { copy, useAction, useChannels, useInvalidateAll } from '../composables'
import { floating } from '../safe-area'

const props = defineProps<{ slug: string }>()

const toast = useToast()
const run = useAction()
const router = useRouter()
const invalidate = useInvalidateAll()
const { data, isPending, error } = useQuery({
  key: () => ['playlist', props.slug],
  query: () => api.playlist(props.slug)
})
const { data: channels } = useChannels()

type Item = PlaylistDetail['items'][number]
/** Local copy so drag-and-drop feels instant. `confirmed` is the last order the server accepted. */
const items = ref<Item[]>([])
let confirmed: Item[] = []
const saving = ref(false)
/** Bumped on every save and on route change, so late responses for another playlist are ignored. */
let op = 0

watch(
  data,
  (d) => {
    if (saving.value || !d || d.slug !== props.slug) return
    confirmed = [...d.items]
    items.value = [...d.items]
  },
  { immediate: true }
)
watch(
  () => props.slug,
  () => {
    op++
    saving.value = false
    // Cached data for the new slug (if any) is the confirmed state; keep both in step.
    confirmed = data.value && data.value.slug === props.slug ? [...data.value.items] : []
    items.value = [...confirmed]
  }
)

/** Replace the whole order. Order-changing controls are disabled while a save is in flight. */
async function save(ids: number[], message?: string) {
  if (saving.value) return
  const slug = props.slug
  const mine = ++op
  saving.value = true
  const r = await run(() => api.replaceItems(slug, ids), message)
  if (mine !== op || slug !== props.slug) return // superseded or navigated away
  saving.value = false
  if (r.ok) {
    confirmed = [...r.value.items]
    items.value = [...r.value.items]
  } else {
    items.value = [...confirmed] // roll back the optimistic change immediately
  }
  await invalidate()
}

function onDragEnd() {
  const ids = items.value.map((i) => i.channel.id)
  const before = data.value?.items.map((i) => i.channel.id) ?? []
  if (ids.join() !== before.join()) save(ids)
}

async function remove(item: Item) {
  await save(
    items.value.filter((i) => i.channel.id !== item.channel.id).map((i) => i.channel.id),
    `Removed ${item.channel.name}`
  )
}

const toAdd = ref<number[]>([])
const addable = computed(() => {
  const inList = new Set(items.value.map((i) => i.channel.id))
  return (channels.value ?? [])
    .filter((c) => !inList.has(c.id))
    .map((c) => ({ label: c.name, value: c.id, description: c.group_title ?? undefined }))
})
async function addSelected() {
  if (!toAdd.value.length) return
  const n = toAdd.value.length
  await save([...items.value.map((i) => i.channel.id), ...toAdd.value], `Added ${n} channel${n > 1 ? 's' : ''}`)
  toAdd.value = []
}

// rename / delete
const settingsOpen = ref(false)
const edit = reactive({ name: '', slug: '' })
watch(settingsOpen, (o) => {
  if (o && data.value) {
    edit.name = data.value.name
    edit.slug = data.value.slug
  }
})
async function saveSettings() {
  const r = await run(() => api.patchPlaylist(props.slug, { name: edit.name, slug: edit.slug }), 'Playlist saved')
  if (r.ok) {
    settingsOpen.value = false
    await invalidate()
    if (r.value.slug !== props.slug) router.replace(`/playlists/${r.value.slug}`)
  }
}
const deleteOpen = ref(false)
async function deletePlaylist() {
  deleteOpen.value = false
  if ((await run(() => api.deletePlaylist(props.slug), `Deleted ${props.slug}`)).ok) {
    await invalidate()
    router.push('/playlists')
  }
}
</script>

<template>
  <UDashboardPanel :id="`playlist-${slug}`">
    <template #header>
      <UDashboardNavbar :title="data?.name ?? slug">
        <template #leading>
          <UButton icon="i-lucide-arrow-left" color="neutral" variant="ghost" to="/playlists" aria-label="Back" />
        </template>
        <template #right>
          <UButton
            icon="i-lucide-link"
            color="neutral"
            variant="subtle"
            label="M3U"
            aria-label="Copy M3U URL"
            @click="copy(playlistUrl(slug), toast)"
          />
          <UButton
            icon="i-lucide-calendar-range"
            color="neutral"
            variant="subtle"
            label="Guide"
            aria-label="Copy guide URL"
            @click="copy(guideUrl(slug), toast)"
          />
          <UButton icon="i-lucide-settings-2" color="neutral" variant="ghost" aria-label="Playlist settings" @click="settingsOpen = true" />
        </template>
      </UDashboardNavbar>
      <UDashboardToolbar>
        <template #left>
          <USelectMenu
            v-model="toAdd"
            :content="floating()"
            :items="addable"
            value-key="value"
            multiple
            placeholder="Add channels…"
            class="w-full sm:w-80"
          />
          <UButton label="Add" :disabled="!toAdd.length || saving" :loading="saving" @click="addSelected" />
        </template>
      </UDashboardToolbar>
    </template>

    <template #body>
      <UAlert v-if="error" color="error" variant="subtle" :title="error.message" />
      <div v-else-if="isPending" class="text-muted">Loading…</div>
      <div v-else-if="!items.length" class="py-10 text-center text-muted">This playlist is empty.</div>

      <p v-if="items.length" class="text-sm text-muted mb-2">
        Drag to reorder. The number is the channel number players show (tvg-chno).
      </p>
      <VueDraggable
        v-model="items"
        handle=".drag-handle"
        :disabled="saving"
        :animation="150"
        class="flex flex-col divide-y divide-default border border-default rounded-md"
        @end="onDragEnd"
      >
        <div
          v-for="(item, i) in items"
          :key="item.channel.id"
          class="flex items-center gap-3 px-3 py-2 bg-default"
          :class="{ 'opacity-60': !item.channel.enabled }"
        >
          <UIcon name="i-lucide-grip-vertical" class="drag-handle size-5 text-dimmed cursor-grab touch-none" />
          <span class="w-8 text-right font-mono text-muted">{{ i + 1 }}</span>
          <img v-if="item.channel.tvg_logo" :src="item.channel.tvg_logo" alt="" class="size-6 object-contain" loading="lazy" />
          <UIcon v-else name="i-lucide-tv" class="size-6 text-dimmed" />
          <div class="min-w-0 flex-1">
            <div class="truncate font-medium">
              {{ item.channel.name }}
              <span v-if="!item.channel.enabled" class="text-xs text-muted">(disabled)</span>
            </div>
            <div class="text-xs text-muted truncate">
              {{ item.channel.group_title ?? '—' }} · {{ sourceLabel(item.channel.source) }}
            </div>
          </div>
          <UButton
            icon="i-lucide-x"
            color="neutral"
            variant="ghost"
            size="sm"
            :aria-label="`Remove ${item.channel.name}`"
            :disabled="saving"
            @click="remove(item)"
          />
        </div>
      </VueDraggable>

      <UModal v-model:open="settingsOpen" title="Playlist settings">
        <template #body>
          <div class="flex flex-col gap-4">
            <UFormField label="Name"><UInput v-model="edit.name" class="w-full" /></UFormField>
            <UFormField label="Slug" help="Changing it changes the playlist URL (update your players too)">
              <UInput v-model="edit.slug" class="w-full" autocapitalize="off" />
            </UFormField>
          </div>
        </template>
        <template #footer>
          <div class="flex justify-between gap-2 w-full">
            <UButton color="error" variant="ghost" label="Delete playlist" @click="(settingsOpen = false), (deleteOpen = true)" />
            <div class="flex gap-2">
              <UButton color="neutral" variant="ghost" label="Cancel" @click="settingsOpen = false" />
              <UButton label="Save" @click="saveSettings" />
            </div>
          </div>
        </template>
      </UModal>

      <UModal v-model:open="deleteOpen" :title="`Delete ${slug}?`" description="Channels are kept; only the playlist is removed.">
        <template #footer>
          <div class="flex justify-end gap-2 w-full">
            <UButton color="neutral" variant="ghost" label="Cancel" @click="deleteOpen = false" />
            <UButton color="error" label="Delete" @click="deletePlaylist" />
          </div>
        </template>
      </UModal>
    </template>
  </UDashboardPanel>
</template>
