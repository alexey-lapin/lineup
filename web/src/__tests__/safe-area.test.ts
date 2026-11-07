import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
import { keepPageAtTop } from '../safe-area'

describe('keepPageAtTop', () => {
  let a: HTMLInputElement
  let b: HTMLInputElement
  let scrollTo: ReturnType<typeof vi.spyOn>

  beforeAll(() => keepPageAtTop(300))

  beforeEach(() => {
    vi.useFakeTimers()
    document.body.innerHTML = '<input id="a"><input id="b"><button id="c">x</button>'
    a = document.getElementById('a') as HTMLInputElement
    b = document.getElementById('b') as HTMLInputElement
    Object.defineProperty(window, 'scrollY', { value: 62, configurable: true })
    scrollTo = vi.spyOn(window, 'scrollTo').mockImplementation(() => {})
  })

  afterEach(() => {
    vi.useRealTimers()
    vi.restoreAllMocks()
  })

  it('leaves the page alone when focus moves from one field to the next', () => {
    a.focus()
    b.focus()
    vi.advanceTimersByTime(1000)
    expect(document.activeElement).toBe(b)
    expect(scrollTo).not.toHaveBeenCalled()
  })

  it('drops a pending reset when the field is refocused', () => {
    a.focus()
    a.blur()
    vi.advanceTimersByTime(100)
    a.focus()
    vi.advanceTimersByTime(1000)
    expect(scrollTo).not.toHaveBeenCalled()
  })

  it('scrolls the page back once editing ends', () => {
    a.focus()
    a.blur()
    vi.advanceTimersByTime(299)
    expect(scrollTo).not.toHaveBeenCalled()
    vi.advanceTimersByTime(1)
    expect(scrollTo).toHaveBeenCalledWith(0, 0)
  })

  it('scrolls back when focus moves from a field to a button', () => {
    a.focus()
    ;(document.getElementById('c') as HTMLButtonElement).focus()
    vi.advanceTimersByTime(300)
    expect(scrollTo).toHaveBeenCalledWith(0, 0)
  })
})
