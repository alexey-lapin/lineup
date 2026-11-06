<script setup lang="ts">
import { ref, watch } from 'vue'
import { api, type SettingsTestResult, type SettingsTestTarget } from '../../api'

const props = defineProps<{ label: string; help: string; target: SettingsTestTarget; placeholder: string }>()
const model = defineModel<string>({ required: true })

const testing = ref(false)
const result = ref<(SettingsTestResult & { forValue: string }) | null>(null)
const failed = ref('')
let seq = 0

// A result belongs to the value it tested; editing drops it, and late answers are ignored.
watch(model, () => {
  seq++
  testing.value = false
  result.value = null
  failed.value = ''
})

async function test() {
  const value = model.value.trim()
  const mine = ++seq
  testing.value = true
  result.value = null
  failed.value = ''
  try {
    const r = await api.testSetting(props.target, value)
    if (mine === seq) result.value = { ...r, forValue: value }
  } catch (e) {
    if (mine === seq) failed.value = (e as Error).message
  } finally {
    if (mine === seq) testing.value = false
  }
}
</script>

<template>
  <UFormField :label="label" :help="help">
    <div class="flex flex-col sm:flex-row gap-2">
      <UInput
        v-model="model"
        type="url"
        inputmode="url"
        autocapitalize="off"
        autocorrect="off"
        spellcheck="false"
        :placeholder="placeholder"
        class="flex-1 min-w-0"
        size="lg"
      />
      <UButton
        icon="i-lucide-plug-zap"
        color="neutral"
        variant="subtle"
        size="lg"
        label="Test"
        :loading="testing"
        :disabled="!model.trim()"
        class="justify-center"
        @click="test"
      />
    </div>
  </UFormField>
  <p role="status" aria-live="polite" class="text-sm -mt-2 break-all empty:hidden">
    <template v-if="result">
      <span class="flex items-start gap-1.5" :class="result.ok ? 'text-success' : 'text-error'">
        <UIcon :name="result.ok ? 'i-lucide-circle-check' : 'i-lucide-circle-x'" class="size-4 mt-0.5 shrink-0" />
        <span>{{ result.detail }}</span>
      </span>
      <span class="block text-xs text-dimmed mt-0.5">
        {{ result.url }} · {{ result.elapsed_ms }} ms · tested from the lineup server
      </span>
    </template>
    <span v-else-if="failed" class="text-error">{{ failed }}</span>
  </p>
</template>
