import { reactive } from 'vue'

/**
 * The iPhone safe-area insets in pixels (0 elsewhere), measured from the `--safe-*` CSS variables
 * (main.css) and kept current across rotation and resizing.
 *
 * Fixed surfaces get the insets in CSS (vite.config.ts theme). Floating menus can't: their
 * positioning only avoids the viewport edge, so they take `floating()` as their `content` prop.
 */
export const insets = reactive({ top: 0, right: 0, bottom: 0, left: 0 })

const SIDES = ['top', 'right', 'bottom', 'left'] as const
/** Nuxt UI's default gap between a menu and the screen edge. */
const GAP = 8

function measure(probe: HTMLElement) {
  const style = getComputedStyle(probe)
  for (const side of SIDES) {
    insets[side] = parseFloat(style.getPropertyValue(`padding-${side}`)) || 0
  }
}

/** Start measuring; call once at startup. */
export function watchSafeArea() {
  const probe = document.createElement('div')
  probe.setAttribute('aria-hidden', 'true')
  probe.style.cssText =
    'position:fixed;visibility:hidden;pointer-events:none;top:0;left:0;' +
    'padding:var(--safe-top) var(--safe-right) var(--safe-bottom) var(--safe-left)'
  document.body.append(probe)
  const update = () => measure(probe)
  update()
  window.addEventListener('resize', update)
  window.addEventListener('orientationchange', update)
}

/** `content` props for dropdowns, selects and tooltips: keep clear of the status bar and home indicator. */
export function floating<T extends object>(extra?: T) {
  return {
    collisionPadding: {
      top: insets.top + GAP,
      right: insets.right + GAP,
      bottom: insets.bottom + GAP,
      left: insets.left + GAP
    },
    ...extra
  }
}
