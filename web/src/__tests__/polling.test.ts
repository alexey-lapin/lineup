import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { nextTick, ref } from 'vue'
import { deferred, flush } from './setup'
import { usePolling } from '../composables'

function setHidden(hidden: boolean) {
  Object.defineProperty(document, 'hidden', { configurable: true, get: () => hidden })
  document.dispatchEvent(new Event('visibilitychange'))
}

/** Mount a host running usePolling; `fetch` sees each call's key and signal. */
function host(key = ref<string | undefined>('a')) {
  const calls: { key: string; signal: AbortSignal; d: ReturnType<typeof deferred<string>> }[] = []
  const fetch = (k: string, signal: AbortSignal) => {
    const d = deferred<string>()
    calls.push({ key: k, signal, d })
    return d.promise
  }
  let poll!: ReturnType<typeof usePolling<string, string>>
  const w = mount({ template: '<div/>', setup: () => void (poll = usePolling(() => key.value, fetch, 5000)) })
  return { w, poll, calls, key }
}

beforeEach(() => setHidden(false))
afterEach(() => vi.useRealTimers())

describe('usePolling', () => {
  it('never overlaps: a refresh during a request runs once after it', async () => {
    const { poll, calls } = host()
    expect(calls).toHaveLength(1)
    void poll.refresh()
    void poll.refresh()
    expect(calls).toHaveLength(1)
    calls[0]!.d.resolve('one')
    await flush()
    expect(poll.data.value).toBe('one')
    expect(calls).toHaveLength(2)
  })

  it('aborts and drops the request in flight when the page is hidden, and asks again when shown', async () => {
    const { poll, calls } = host()
    setHidden(true)
    expect(calls[0]!.signal.aborted).toBe(true)
    calls[0]!.d.resolve('late')
    await flush()
    expect(poll.data.value).toBeUndefined()
    setHidden(false)
    expect(calls).toHaveLength(2)
  })

  it('drops answers for an old key and clears its data', async () => {
    const { poll, calls, key } = host()
    calls[0]!.d.resolve('a1')
    await flush()
    vi.useFakeTimers()
    key.value = 'b'
    await nextTick()
    expect(poll.data.value).toBeUndefined()
    expect(calls.at(-1)!.key).toBe('b')
    vi.useRealTimers()
  })

  it('keeps the last data but reports a failure', async () => {
    const { poll, calls } = host()
    calls[0]!.d.resolve('ok')
    await flush()
    void poll.refresh()
    calls[1]!.d.reject(new Error('server gone'))
    await flush()
    expect([poll.data.value, poll.error.value]).toEqual(['ok', 'Error: server gone'])
  })

  it('stops on unmount', async () => {
    const { w, calls } = host()
    w.unmount()
    expect(calls[0]!.signal.aborted).toBe(true)
  })
})
