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

  keepPageAtTop()
}

/** True while a text field or other editable element has focus (the software keyboard is up). */
function editing() {
  const el = document.activeElement
  if (!(el instanceof HTMLElement)) return false
  return (
    el.isContentEditable ||
    el.matches('input:not([type=checkbox],[type=radio],[type=button],[type=submit]), textarea, select')
  )
}

/**
 * The page never scrolls (only the panels do), but iOS still scrolls it to bring a focused field
 * above the keyboard and doesn't always scroll it back, which leaves the app shifted. Once editing
 * ends, put it back. Moving from one field to the next is not the end: the browser keeps panning
 * for the new field, so a pending reset is dropped when focus lands on it.
 */
export function keepPageAtTop(delay = 300) {
  let timer: ReturnType<typeof setTimeout> | undefined
  const reset = () => {
    timer = undefined
    if (!editing() && (window.scrollX || window.scrollY)) window.scrollTo(0, 0)
  }
  document.addEventListener('focusin', () => {
    if (!editing()) return
    clearTimeout(timer)
    timer = undefined
  })
  // Wait for the keyboard to finish closing; the viewport resize below catches a slower close.
  document.addEventListener('focusout', () => {
    clearTimeout(timer)
    timer = setTimeout(reset, delay)
  })
  window.visualViewport?.addEventListener('resize', () => {
    if (!editing()) reset()
  })
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
