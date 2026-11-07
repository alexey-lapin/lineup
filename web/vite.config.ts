import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import ui from '@nuxt/ui/vite'
import type { Plugin } from 'vite'

const server = process.env.LINEUP_DEV_SERVER ?? 'http://127.0.0.1:9300'

/** Writes shell.json, the list of every built asset, which the service worker caches for offline use. */
function shellList(): Plugin {
  return {
    name: 'lineup-shell-list',
    apply: 'build',
    generateBundle(_, bundle) {
      const assets = Object.keys(bundle)
        .filter((f) => f.startsWith('assets/'))
        .map((f) => `/${f}`)
        .sort()
      this.emitFile({ type: 'asset', fileName: 'shell.json', source: JSON.stringify({ assets }) })
    }
  }
}

export default defineConfig({
  plugins: [
    shellList(),
    vue(),
    ui({
      ui: {
        colors: { primary: 'emerald', neutral: 'zinc' },
        // iPhone home-screen app: keep fixed surfaces clear of the status bar and home indicator
        // (--safe-* in main.css; all 0 elsewhere).
        dashboardGroup: {
          base: 'pt-(--safe-top) pl-(--safe-left) pr-(--safe-right)'
        },
        // Stretch to the padded group instead of forcing a full viewport height. The body scrolls
        // under the home indicator, with room to scroll its end clear of it.
        dashboardPanel: {
          slots: {
            root: 'min-h-0',
            body: 'pb-[calc(1rem+var(--safe-bottom))] sm:pb-[calc(1.5rem+var(--safe-bottom))]'
          }
        },
        // The persistent sidebar (wide screens; the phone menu is a drawer) keeps its last link clear of
        // the home indicator.
        dashboardSidebar: { slots: { root: 'min-h-0 pb-(--safe-bottom)' } },
        slideover: {
          slots: {
            content: 'pt-(--safe-top) pb-(--safe-bottom) pl-(--safe-left) pr-(--safe-right)',
            close: 'top-[calc(1rem+var(--safe-top))] end-[calc(1rem+var(--safe-right))]'
          }
        },
        // The phone sidebar menu is a bottom drawer.
        drawer: {
          slots: { content: 'pb-(--safe-bottom) pl-(--safe-left) pr-(--safe-right)' },
          compoundVariants: [
            {
              direction: ['top', 'bottom'],
              class: { content: 'max-h-[calc(96%-var(--safe-top)-var(--safe-bottom))]' }
            }
          ]
        },
        // Centred dialogs: centre within the safe area and fit inside it.
        modal: {
          compoundVariants: [
            {
              scrollable: false,
              fullscreen: false,
              class: {
                content:
                  'top-[calc(50%+(var(--safe-top)-var(--safe-bottom))/2)] ' +
                  'max-h-[calc(100dvh-2rem-var(--safe-top)-var(--safe-bottom))] ' +
                  'sm:max-h-[calc(100dvh-4rem-var(--safe-top)-var(--safe-bottom))]'
              }
            }
          ]
        },
        toaster: {
          slots: { viewport: 'mt-(--safe-top) mb-(--safe-bottom) ml-(--safe-left) mr-(--safe-right)' }
        }
      }
    })
  ],
  server: {
    proxy: {
      '/api': server,
      '/p/': server
    }
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true
  }
})
