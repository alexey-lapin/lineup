import { createRouter, createWebHistory } from 'vue-router'

export const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', redirect: '/channels' },
    { path: '/channels', component: () => import('./pages/ChannelsPage.vue') },
    { path: '/playlists', component: () => import('./pages/PlaylistsPage.vue') },
    { path: '/playlists/:slug', component: () => import('./pages/PlaylistPage.vue'), props: true },
    { path: '/epg', component: () => import('./pages/EpgPage.vue') },
    { path: '/hooks', component: () => import('./pages/HooksPage.vue') },
    { path: '/import', component: () => import('./pages/ImportPage.vue') },
    { path: '/integrations', component: () => import('./pages/IntegrationsPage.vue') },
    { path: '/settings', component: () => import('./pages/SettingsPage.vue') },
    { path: '/:pathMatch(.*)*', redirect: '/channels' }
  ]
})
