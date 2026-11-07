# Plan: iPhone home-screen layout (safe areas)

Repo: lineup, web/ (Vue 3 + Nuxt UI 4, Vite plugin `ui()` in vite.config.ts).

## Problem (owner report, reproduced)

Added to the iPhone home screen, the app draws under the status bar / Dynamic Island and the home
indicator; header buttons (sidebar toggle, "Add") are not tappable, the bottom is covered.

Reproduced in Chrome with iPhone 16 Pro emulation (402x874) plus CDP
`Emulation.setSafeAreaInsetsOverride {top: 62, bottom: 34}`, so `env(safe-area-inset-*)` resolves like a
standalone PWA. Screenshots: scratchpad ios/before-channels.png, before-settings.png
(a local scratch dir).

Causes:
- index.html has `viewport-fit=cover` + `apple-mobile-web-app-status-bar-style=black-translucent`, so
  the page is edge to edge.
- main.css pads `body` with the insets, which does nothing: `UDashboardGroup` is `fixed inset-0`.
- `UDashboardPanel` root is `min-h-svh`; inside a padded group that would overflow and be clipped.
- Overlays are portaled and `fixed` with no inset handling: the sidebar menu (UDrawer, left, mode
  "drawer"), USlideover (right, full width on phones; close button `absolute top-4 end-4`), UModal
  (centered, `max-h-[calc(100dvh-2rem)]`), UToaster (bottom-right, `bottom-4`), dropdown/select popovers.
- Light colour scheme follows the system; with black-translucent the status bar text is white, so on a
  light page the clock/battery are unreadable.

## Fix

1. main.css: define `--safe-top/right/bottom/left: env(safe-area-inset-*, 0px)` on :root; drop the body
   padding. Everything below uses the variables (one place to reason about; the browser test overrides
   nothing, it emulates real insets via CDP).
2. Theme overrides in `ui({ ui: { ... } })` (tailwind-merge appends/replaces):
   - dashboardGroup.base: add `pt-(--safe-top) pb-(--safe-bottom) pl-(--safe-left) pr-(--safe-right)`.
   - dashboardPanel.slots.root and dashboardSidebar.slots.root: `min-h-0` instead of `min-h-svh`
     (the group is a flex row; children stretch to its padded height).
   - slideover.slots.content: pad all four insets; close button offset by the insets
     (`top-[calc(1rem+var(--safe-top))] end-[calc(1rem+var(--safe-right))]`).
   - drawer.slots.content: pad all four (only the left sidebar drawer is used).
   - modal: non-fullscreen max-h becomes `calc(100dvh - 2rem - var(--safe-top) - var(--safe-bottom))`;
     centred, so with 62/34 insets the box clears both (top edge >= 62).
   - toaster.slots.viewport: margins `mt-(--safe-top) mb-(--safe-bottom) ml-(--safe-left) mr-(--safe-right)`.
   - popovers (dropdownMenu/selectMenu/select/popover/tooltip content): Reka `collisionPadding` can't be
     set from the theme; leave as is unless you see a concrete case (they flip/shift within the viewport,
     8 px default padding, could end up under the home indicator).
3. Status bar contrast: keep `black-translucent` (edge to edge, works with dark mode) and paint the top
   inset band dark in both schemes with a fixed `body::before` of height `var(--safe-top)`, background
   `#09090b`, so white status text is always readable. Alternative: `default` status-bar style (iOS
   draws its own bar, top inset becomes 0) but it is white in dark mode and Apple's handling of
   theme-color for standalone apps varies by iOS version. Recommend the band.
4. Landscape: left/right insets handled by the same padding.

## Verification

agent-browser, iPhone 16 Pro emulation + the CDP inset override, light and dark: each page
(Channels, Playlists, EPG, Hooks, Import, Settings) scrolled to the bottom, sidebar menu open, the
channel slideover, a modal, a toast; check header buttons are below 62 px and the last control above
874-34; tap them via agent-browser. Landscape (874x402, insets left/right 62, bottom 21). Desktop
(1280x800, insets 0) unchanged. pnpm build, vue-tsc, tests.

## Questions

1. Is the theme-override approach complete for Nuxt UI 4 here (anything else fixed/portaled)?
2. Status bar: dark band vs `default` style?
3. Any risk in `min-h-0` on panel/sidebar roots (desktop layout, resizable sidebar)?
