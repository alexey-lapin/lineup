<script setup lang="ts">
import type { ConnectionTestResult } from '../../api'

/**
 * The live answer of a component at its saved URL (nothing while it is not configured). A failed
 * poll wins over an older answer, which may no longer be true.
 */
defineProps<{ result?: ConnectionTestResult; error?: string }>()
</script>

<template>
  <p v-if="result || error" role="status" aria-live="polite" class="text-sm flex items-start gap-1.5">
    <template v-if="result && !error">
      <UIcon
        :name="result.ok ? 'i-lucide-circle-check' : 'i-lucide-circle-alert'"
        class="size-4 mt-0.5 shrink-0"
        :class="result.ok ? 'text-success' : 'text-error'"
      />
      <span>
        <span class="text-muted">Saved URL:</span>
        {{ result.ok ? result.detail : `not answering (${result.detail})` }}
      </span>
    </template>
    <template v-else>
      <UIcon name="i-lucide-circle-help" class="size-4 mt-0.5 shrink-0 text-muted" />
      <span class="text-muted">Status unavailable: {{ error }}</span>
    </template>
  </p>
</template>
