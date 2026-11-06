<script setup lang="ts">
import type { SettingsSection } from './section'

defineProps<{ title: string; icon: string; description?: string; section: SettingsSection; loading?: boolean }>()
const emit = defineEmits<{ saved: [] }>()
</script>

<template>
  <UCard :ui="{ body: 'p-4 sm:p-5 flex flex-col gap-4' }">
    <div class="flex items-start gap-3">
      <UIcon :name="icon" class="size-5 mt-0.5 shrink-0 text-muted" />
      <div class="min-w-0 flex-1">
        <div class="flex items-center gap-2 flex-wrap">
          <h3 class="font-semibold">{{ title }}</h3>
          <UBadge v-if="section.dirty" color="warning" variant="subtle" size="sm" label="Unsaved" />
        </div>
        <p v-if="description" class="text-sm text-muted">{{ description }}</p>
      </div>
    </div>

    <fieldset :disabled="loading || section.pending" class="flex flex-col gap-4 min-w-0">
      <slot />
    </fieldset>

    <UAlert
      v-if="section.conflict"
      color="warning"
      variant="subtle"
      icon="i-lucide-refresh-ccw"
      title="Changed on the server while you were editing"
      description="Reset to see the current values, or save to keep yours."
    />
    <UAlert v-if="section.error" color="error" variant="subtle" icon="i-lucide-circle-x" :title="section.error" />

    <div class="flex flex-wrap items-center gap-3 justify-between">
      <div class="flex flex-wrap gap-2">
        <slot name="actions" />
      </div>
      <div class="flex gap-3 ml-auto">
        <UButton
          color="neutral"
          variant="ghost"
          label="Reset"
          size="lg"
          :disabled="!section.dirty && !section.conflict"
          @click="section.reset()"
        />
        <UButton
          label="Save"
          size="lg"
          :loading="section.pending"
          :disabled="!section.dirty"
          @click="section.save().then((ok) => ok && emit('saved'))"
        />
      </div>
    </div>
  </UCard>
</template>
