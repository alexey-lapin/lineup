<script setup lang="ts">
import { onBeforeUnmount, ref, watch } from 'vue'
import { api, type EpgChannel } from '../api'

const props = defineProps<{ initial?: string; actionLabel: string; actionIcon: string }>()
const emit = defineEmits<{ select: [c: EpgChannel] }>()

const query = ref(props.initial ?? '')
const results = ref<EpgChannel[]>([])
const searching = ref(false)
const failed = ref('')

let seq = 0
let debounce: ReturnType<typeof setTimeout> | undefined
watch(
  query,
  (q) => {
    clearTimeout(debounce)
    const mine = ++seq
    failed.value = ''
    if (!q.trim()) {
      results.value = []
      searching.value = false
      return
    }
    searching.value = true
    debounce = setTimeout(async () => {
      try {
        const r = await api.epgChannels(q.trim(), 30)
        if (mine === seq) results.value = r
      } catch (e) {
        if (mine === seq) {
          results.value = []
          failed.value = (e as Error).message
        }
      } finally {
        if (mine === seq) searching.value = false
      }
    }, 200)
  },
  { immediate: true }
)
onBeforeUnmount(() => clearTimeout(debounce))
</script>

<template>
  <div class="flex flex-col gap-3">
    <UInput
      v-model="query"
      icon="i-lucide-search"
      placeholder="Name or id, e.g. sports one"
      class="w-full"
      :loading="searching"
      autocapitalize="off"
      autocomplete="off"
    />
    <ul v-if="results.length" class="divide-y divide-default border border-default rounded-md">
      <li v-for="c in results" :key="`${c.source_id}:${c.xmltv_id}`" class="flex items-center gap-3 p-2">
        <img v-if="c.icon" :src="c.icon" alt="" class="size-8 object-contain shrink-0" loading="lazy" />
        <UIcon v-else name="i-lucide-tv" class="size-8 p-1.5 text-dimmed shrink-0" />
        <div class="min-w-0 flex-1">
          <div class="font-mono text-sm truncate">{{ c.xmltv_id }}</div>
          <div class="text-sm text-muted truncate">{{ c.display_names.join(' · ') }}</div>
        </div>
        <span v-if="c.score != null" class="text-xs text-dimmed">{{ c.score.toFixed(2) }}</span>
        <UButton
          :icon="actionIcon"
          color="neutral"
          variant="ghost"
          size="sm"
          :aria-label="actionLabel"
          @click="emit('select', c)"
        />
      </li>
    </ul>
    <p v-else-if="failed" class="text-sm text-error">{{ failed }}</p>
    <p v-else-if="query.trim() && !searching" class="text-sm text-muted">
      No matches. Guide channels appear once a source under EPG has been fetched.
    </p>
  </div>
</template>
