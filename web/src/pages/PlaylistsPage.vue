<script setup lang="ts">
import { useToast } from '@nuxt/ui/composables'
import { reactive, ref } from 'vue'
import { useRouter } from 'vue-router'
import { api, guideUrl, playlistUrl } from '../api'
import { copy, useAction, useInvalidateAll, usePlaylists } from '../composables'

const toast = useToast()
const run = useAction()
const router = useRouter()
const invalidate = useInvalidateAll()
const { data, isPending, error } = usePlaylists()

const createOpen = ref(false)
const form = reactive({ slug: '', name: '' })

async function create() {
  const slug = form.slug.trim()
  const r = await run(() => api.createPlaylist(slug, form.name.trim()), `Created ${slug}`)
  if (r.ok) {
    await invalidate()
    createOpen.value = false
    form.slug = ''
    form.name = ''
    router.push(`/playlists/${slug}`)
  }
}
</script>

<template>
  <UDashboardPanel id="playlists">
    <template #header>
      <UDashboardNavbar title="Playlists">
        <template #leading>
          <UDashboardSidebarCollapse />
        </template>
        <template #right>
          <UButton icon="i-lucide-plus" label="New" @click="createOpen = true" />
        </template>
      </UDashboardNavbar>
    </template>

    <template #body>
      <UAlert v-if="error" color="error" variant="subtle" :title="error.message" />
      <div v-if="isPending" class="text-muted">Loading…</div>
      <div v-else-if="!data?.length" class="py-10 text-center text-muted">
        No playlists yet. Create one, or import your existing m3u.
      </div>
      <div class="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
        <UCard v-for="p in data" :key="p.slug" :ui="{ body: 'p-4 sm:p-4' }">
          <div class="flex items-start justify-between gap-3">
            <RouterLink :to="`/playlists/${p.slug}`" class="min-w-0 group">
              <div class="font-semibold group-hover:text-primary truncate">{{ p.name }}</div>
              <div class="text-sm text-muted">{{ p.slug }} · {{ p.channel_count }} channels</div>
            </RouterLink>
            <div class="flex gap-2 shrink-0">
              <UButton
                icon="i-lucide-link"
                color="neutral"
                variant="subtle"
                size="sm"
                label="M3U"
                @click="copy(playlistUrl(p.slug), toast)"
              />
              <UButton
                icon="i-lucide-calendar-range"
                color="neutral"
                variant="subtle"
                size="sm"
                label="Guide"
                @click="copy(guideUrl(p.slug), toast)"
              />
            </div>
          </div>
        </UCard>
      </div>

      <UModal v-model:open="createOpen" title="New playlist">
        <template #body>
          <form class="flex flex-col gap-4" @submit.prevent="create">
            <UFormField label="Slug" help="Used in the URL: lowercase letters, digits, - and _" required>
              <UInput v-model="form.slug" class="w-full" placeholder="jellyfin" autocapitalize="off" />
            </UFormField>
            <UFormField label="Name">
              <UInput v-model="form.name" class="w-full" placeholder="Jellyfin" />
            </UFormField>
            <button type="submit" class="hidden" />
          </form>
        </template>
        <template #footer>
          <div class="flex justify-end gap-2 w-full">
            <UButton color="neutral" variant="ghost" label="Cancel" @click="createOpen = false" />
            <UButton label="Create" :disabled="!form.slug.trim()" @click="create" />
          </div>
        </template>
      </UModal>
    </template>
  </UDashboardPanel>
</template>
