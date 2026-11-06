// lineup service worker: caches the UI shell (index.html and every built asset, listed in
// shell.json at build time) so the home-screen app opens offline, on any page. API calls, playlists and guides (/api, /p) always go to the network.
const CACHE = 'lineup-shell-v4'
const STATIC = ['/icon.svg', '/manifest.webmanifest', '/apple-touch-icon.png']

/** Asset URLs (/assets/...) that an index.html references directly. */
function assetsOf(html) {
  return [...new Set([...html.matchAll(/["'](\/assets\/[^"'?#]+)["']/g)].map((m) => m[1]))]
}

/**
 * Store this index.html with every asset of its build, then drop assets of older builds. The
 * index is stored last, so a cached index never points at assets missing from the cache. If the
 * index and shell.json come from different builds (a deploy in between), nothing changes.
 */
async function refreshShell(indexResponse) {
  const html = await indexResponse.clone().text()
  const list = await fetch('/shell.json', { cache: 'no-store' })
  if (!list.ok) throw new Error(`shell.json: HTTP ${list.status}`)
  const { assets } = await list.json()
  if (!assetsOf(html).every((a) => assets.includes(a))) {
    throw new Error('index.html and shell.json are from different builds')
  }
  const cache = await caches.open(CACHE)
  const missing = []
  for (const a of assets) {
    if (!(await cache.match(a))) missing.push(a)
  }
  await cache.addAll(missing)
  await cache.put('/', indexResponse)
  for (const req of await cache.keys()) {
    const path = new URL(req.url).pathname
    if (path.startsWith('/assets/') && !assets.includes(path)) await cache.delete(req)
  }
}

/** Refreshes run one at a time, so one run's cleanup never removes what another just cached. */
let queue = Promise.resolve()
function cacheShell(indexResponse) {
  const run = queue.then(() => refreshShell(indexResponse))
  queue = run.catch(() => {})
  return run
}

self.addEventListener('install', (event) => {
  event.waitUntil(
    (async () => {
      const cache = await caches.open(CACHE)
      await cache.addAll(STATIC)
      const index = await fetch('/', { cache: 'no-store' })
      if (!index.ok) throw new Error(`index.html: HTTP ${index.status}`)
      await cacheShell(index)
      await self.skipWaiting()
    })()
  )
})

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim())
  )
})

self.addEventListener('fetch', (event) => {
  const req = event.request
  const url = new URL(req.url)
  if (req.method !== 'GET' || url.origin !== self.location.origin) return
  if (url.pathname.startsWith('/api/') || url.pathname.startsWith('/p/') || url.pathname === '/healthz') return

  if (req.mode === 'navigate') {
    // The current index.html when online (refreshing the cached shell), the cached one offline.
    event.respondWith(
      fetch(req)
        .then((r) => {
          if (r.ok) event.waitUntil(cacheShell(r.clone()).catch(() => {}))
          return r
        })
        .catch(async () => (await caches.match('/')) || Response.error())
    )
    return
  }
  // Hashed assets and icons: the cache first, then the network.
  event.respondWith(caches.match(req).then((hit) => hit || fetch(req)))
})
