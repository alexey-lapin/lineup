<script setup lang="ts">
import type { TableColumn } from '@nuxt/ui'
import { computed, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { api, sourceLabel, type ImportReport } from '../api'
import { useAction, useInvalidateAll } from '../composables'

type Entry = ImportReport['entries'][number]

const run = useAction()
const router = useRouter()
const invalidate = useInvalidateAll()

const text = ref('')
const slug = ref('jellyfin')
/** The preview and the exact (text, slug) it was computed for; commit sends that snapshot. */
const preview = ref<ImportReport | null>(null)
let snapshot: { text: string; slug: string } | null = null
const busy = ref(false)
let seq = 0

const previewIsCurrent = computed(
  () => !!preview.value && !!snapshot && snapshot.text === text.value && snapshot.slug === slug.value.trim()
)
// Any edit invalidates the preview and any in-flight preview request.
watch([text, slug], () => {
  if (!previewIsCurrent.value) {
    seq++
    busy.value = false
    preview.value = null
    snapshot = null
  }
})

async function onFile(e: Event) {
  const f = (e.target as HTMLInputElement).files?.[0]
  if (f) text.value = await f.text()
}

async function doPreview() {
  const snap = { text: text.value, slug: slug.value.trim() }
  const mine = ++seq
  busy.value = true
  const r = await run(() => api.importM3u(snap.text, snap.slug, true))
  if (mine !== seq) return
  busy.value = false
  if (r.ok) {
    snapshot = snap
    preview.value = r.value
  }
}

async function commit() {
  if (!previewIsCurrent.value || !snapshot) return
  const snap = snapshot
  busy.value = true
  const r = await run(() => api.importM3u(snap.text, snap.slug, false), 'Import complete')
  busy.value = false
  if (r.ok) {
    await invalidate()
    router.push(`/playlists/${r.value.playlist_slug}`)
  }
}

const counts = computed(() => {
  const e = preview.value?.entries ?? []
  return {
    create: e.filter((x) => x.action === 'create').length,
    reuse: e.filter((x) => x.action === 'reuse').length,
    skip: e.filter((x) => x.action === 'skip_in_playlist').length
  }
})

const actionColor = { create: 'success', reuse: 'info', skip_in_playlist: 'neutral' } as const

const columns: TableColumn<Entry>[] = [
  { accessorKey: 'index', header: '#' },
  { accessorKey: 'action', header: 'Action' },
  { accessorKey: 'name', header: 'Name' },
  { id: 'source', header: 'Source' },
  { id: 'diff', header: 'Differs from existing' }
]
</script>

<template>
  <UDashboardPanel id="import">
    <template #header>
      <UDashboardNavbar title="Import m3u">
        <template #leading>
          <UDashboardSidebarCollapse />
        </template>
      </UDashboardNavbar>
    </template>

    <template #body>
      <div class="flex flex-col gap-4 max-w-4xl">
        <p class="text-sm text-muted">
          Existing channels (same acestream id, VK video or URL) are reused and keep their current details. New ones are
          created. Everything is appended to the playlist in file order. Preview first; nothing is saved until you
          import.
        </p>
        <div class="grid gap-4 sm:grid-cols-[1fr_auto]">
          <UFormField label="Playlist" help="Created if it does not exist">
            <UInput v-model="slug" class="w-full" autocapitalize="off" />
          </UFormField>
          <UFormField label="File">
            <input type="file" accept=".m3u,.m3u8,audio/x-mpegurl,text/plain" class="text-sm pt-1.5" @change="onFile" />
          </UFormField>
        </div>
        <UFormField label="Or paste m3u">
          <UTextarea
            v-model="text"
            :rows="8"
            class="w-full font-mono"
            placeholder="#EXTM3U&#10;#EXTINF:-1 tvg-id=&quot;…&quot; group-title=&quot;Sports&quot;,Name&#10;http://…"
           
          />
        </UFormField>
        <div class="flex gap-2">
          <UButton label="Preview" icon="i-lucide-eye" :loading="busy" :disabled="!text.trim() || !slug.trim()" @click="doPreview" />
          <UButton
            v-if="previewIsCurrent && preview"
            label="Import"
            icon="i-lucide-file-down"
            color="success"
            :loading="busy"
            :disabled="!preview.entries.length"
            @click="commit"
          />
        </div>

        <template v-if="preview">
          <div class="text-sm">
            Into <b>{{ preview.playlist_slug }}</b><span v-if="preview.playlist_created"> (new playlist)</span>:
            {{ counts.create }} new, {{ counts.reuse }} reused, {{ counts.skip }} already present.
          </div>
          <UAlert
            v-if="preview.skipped_lines.length"
            color="warning"
            variant="subtle"
            title="Ignored lines"
            :description="preview.skipped_lines.join('\n')"
            :ui="{ description: 'whitespace-pre-wrap break-all' }"
          />
          <UTable :data="preview.entries" :columns="columns">
            <template #action-cell="{ row }">
              <UBadge
                :label="row.original.action === 'skip_in_playlist' ? 'skip' : row.original.action"
                :color="actionColor[row.original.action as keyof typeof actionColor]"
                variant="subtle"
              />
            </template>
            <template #source-cell="{ row }">
              <span class="font-mono text-xs">{{ sourceLabel(row.original.source) }}</span>
            </template>
            <template #diff-cell="{ row }">
              <div v-for="d in row.original.diff" :key="d.field" class="text-xs">
                <span class="text-muted">{{ d.field }}:</span> {{ d.existing ?? '—' }} → {{ d.incoming ?? '—' }}
              </div>
            </template>
          </UTable>
        </template>
      </div>
    </template>
  </UDashboardPanel>
</template>
