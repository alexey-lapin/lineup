// Bundle only the lucide icons the app (and Nuxt UI's defaults) use, so the UI works offline on the LAN.
// Writes src/icons.json, consumed by src/icons.ts via @iconify/vue's addCollection.
import { readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { createRequire } from 'node:module'

const require = createRequire(import.meta.url)
const lucide = JSON.parse(readFileSync(require.resolve('@iconify-json/lucide/icons.json'), 'utf8'))

const names = new Set()
const scan = (text) => {
  for (const m of text.matchAll(/i-lucide-([a-z0-9-]+)/g)) names.add(m[1])
}
const walk = (dir) => {
  for (const f of readdirSync(dir)) {
    const p = join(dir, f)
    if (statSync(p).isDirectory()) walk(p)
    else if (/\.(vue|ts|mjs|js)$/.test(f)) scan(readFileSync(p, 'utf8'))
  }
}
walk('src')
// Nuxt UI default icons (chevrons, close, check, loading, ...).
const uiShared = 'node_modules/@nuxt/ui/dist/shared'
for (const f of readdirSync(uiShared)) scan(readFileSync(join(uiShared, f), 'utf8'))

const icons = {}
const missing = []
for (const n of [...names].sort()) {
  const alias = lucide.aliases?.[n]?.parent
  const icon = lucide.icons[n] ?? (alias && lucide.icons[alias])
  if (icon) icons[n] = icon
  else missing.push(n)
}
const out = { prefix: 'lucide', width: lucide.width, height: lucide.height, icons }
writeFileSync('src/icons.json', JSON.stringify(out))
console.log(`icons: ${Object.keys(icons).length} bundled${missing.length ? `, missing: ${missing.join(', ')}` : ''}`)
