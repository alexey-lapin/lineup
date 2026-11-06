<script setup lang="ts">
import type { NavigationMenuItem } from '@nuxt/ui'
import { useLiveUpdates } from './composables'

useLiveUpdates()

const items: NavigationMenuItem[][] = [
  [
    { label: 'Channels', icon: 'i-lucide-tv', to: '/channels' },
    { label: 'Playlists', icon: 'i-lucide-list-video', to: '/playlists' },
    { label: 'EPG', icon: 'i-lucide-calendar-range', to: '/epg' },
    { label: 'Hooks', icon: 'i-lucide-webhook', to: '/hooks' },
    { label: 'Import', icon: 'i-lucide-file-down', to: '/import' }
  ],
  [{ label: 'Settings', icon: 'i-lucide-settings', to: '/settings' }]
]
</script>

<template>
  <UApp>
    <UDashboardGroup storage="local">
      <UDashboardSidebar collapsible resizable mode="drawer">
        <template #header="{ collapsed }">
          <div class="flex items-center gap-2 px-1 font-semibold">
            <UIcon name="i-lucide-radio-tower" class="size-5 text-primary shrink-0" />
            <span v-if="!collapsed">lineup</span>
          </div>
        </template>
        <template #default="{ collapsed }">
          <UNavigationMenu :collapsed="collapsed" :items="items[0]" orientation="vertical" />
          <UNavigationMenu :collapsed="collapsed" :items="items[1]" orientation="vertical" class="mt-auto" />
        </template>
      </UDashboardSidebar>

      <RouterView />
    </UDashboardGroup>
  </UApp>
</template>
