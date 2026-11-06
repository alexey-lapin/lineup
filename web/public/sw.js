// lineup service worker: caches the UI shell so the home-screen app opens offline.
// API calls, playlists and guides (/api, /p) always go to the network.
const CACHE = 'lineup-shell-v1'
const SHELL = ['/', '/icon.svg', '/manifest.webmanifest']

self.addEventListener('install', (event) => {
  self.skipWaiting()
  event.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL)))
})

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim())
  )
})

async function put(request, response) {
  if (response.ok) {
    const cache = await caches.open(CACHE)
    await cache.put(request, response.clone())
  }
  return response
}

self.addEventListener('fetch', (event) => {
  const req = event.request
  const url = new URL(req.url)
  if (req.method !== 'GET' || url.origin !== self.location.origin) return
  if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/p/') || url.pathname === '/healthz') return

  if (url.pathname.startsWith('/assets/')) {
    // Hashed file names never change: cache first.
    event.respondWith(caches.match(req).then((hit) => hit || fetch(req).then((r) => put(req, r))))
    return
  }
  if (req.mode === 'navigate') {
    // Fresh index.html when online (it names the current assets), the cached one offline.
    event.respondWith(
      fetch(req)
        .then((r) => put('/', r))
        .catch(() => caches.match('/'))
    )
    return
  }
  event.respondWith(
    fetch(req)
      .then((r) => put(req, r))
      .catch(() => caches.match(req))
  )
})
