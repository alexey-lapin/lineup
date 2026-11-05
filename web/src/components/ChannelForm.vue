<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { api, sourceLabel, type Channel, type ResolveResponse } from '../api'
import { useAction, useChannels, useInvalidateAll, usePlaylists } from '../composables'

const props = defineProps<{ channel?: Channel | null }>()
const open = defineModel<boolean>('open', { default: false })

const run = useAction()
const invalidate = useInvalidateAll()
const { data: channels } = useChannels()
const { data: playlists } = usePlaylists()

const editing = computed(() => !!props.channel)

const state = ref({
  input: '',
  name: '',
  group_title: '' as string | undefined,
  tvg_id: '',
  tvg_logo: '',
  is_event: false,
  kickoff: '',
  enabled: true,
  playlists: [] as string[]
})
/** The resolution and the exact input it belongs to; only valid while the input is unchanged. */
const resolved = ref<ResolveResponse | null>(null)
const resolvedFor = ref('')
const resolveError = ref('')
const saving = ref(false)

const groups = computed(() =>
  [...new Set((channels.value ?? []).map((c) => c.group_title).filter((g): g is string => !!g))].sort()
)
const playlistItems = computed(() => (playlists.value ?? []).map((p) => ({ label: p.name, value: p.slug })))

/** `datetime-local` value <-> RFC 3339 UTC. */
function toLocalInput(iso?: string | null) {
  if (!iso) return ''
  const d = new Date(iso)
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`
}
function fromLocalInput(v: string) {
  return v ? new Date(v).toISOString() : null
}

// Any open/close or input edit starts a new resolution "session": late responses are dropped.
let resolveSeq = 0
/** Input of the request in flight, if any. */
let pendingFor = ''
function resetResolution() {
  resolveSeq++
  pendingFor = ''
  resolved.value = null
  resolvedFor.value = ''
  resolveError.value = ''
}

watch(
  open,
  (o) => {
    resetResolution()
    if (!o) return
    const c = props.channel
    state.value.input = ''
    state.value.name = c?.name ?? ''
    state.value.group_title = c?.group_title ?? undefined
    state.value.tvg_id = c?.tvg_id ?? ''
    state.value.tvg_logo = c?.tvg_logo ?? ''
    state.value.is_event = c?.is_event ?? false
    state.value.kickoff = toLocalInput(c?.kickoff_at)
    state.value.enabled = c?.enabled ?? true
    state.value.playlists = playlists.value?.length === 1 && !c ? [playlists.value[0]!.slug] : []
  },
  { immediate: true }
)

watch(
  () => state.value.input.trim(),
  (input) => {
    if (input !== resolvedFor.value && input !== pendingFor) resetResolution()
  }
)

async function resolveInput() {
  const input = state.value.input.trim()
  if (input && input === resolvedFor.value) return
  resetResolution()
  if (!input) return
  pendingFor = input
  const seq = resolveSeq
  try {
    const r = await api.resolve(input)
    if (seq !== resolveSeq || !open.value || state.value.input.trim() !== input) return
    pendingFor = ''
    resolved.value = r
    resolvedFor.value = input
    if (!state.value.name && !editing.value) {
      if (r.source.kind === 'vk') {
        state.value.name = `VK ${r.source.owner_id}_${r.source.video_id}`
        state.value.is_event = true
        state.value.group_title ||= 'VK'
      } else if (r.source.kind === 'acestream') {
        state.value.name = `Acestream ${r.source.id.slice(0, 8)}`
      }
    }
  } catch (e) {
    if (seq === resolveSeq) resolveError.value = (e as Error).message
  }
}

/** A resolution that matches the current input exactly, or null. */
const currentResolution = computed(() =>
  resolved.value && resolvedFor.value === state.value.input.trim() ? resolved.value : null
)

const canSave = computed(
  () =>
    state.value.name.trim().length > 0 &&
    (editing.value || (!!currentResolution.value && currentResolution.value.duplicate_of == null)) &&
    !saving.value
)

async function save() {
  if (!canSave.value) return
  const resolution = currentResolution.value
  saving.value = true
  const common = {
    name: state.value.name,
    group_title: state.value.group_title || null,
    tvg_id: state.value.tvg_id || null,
    tvg_logo: state.value.tvg_logo || null,
    is_event: state.value.is_event,
    kickoff_at: state.value.is_event ? fromLocalInput(state.value.kickoff) : null,
    enabled: state.value.enabled
  }
  const result = props.channel
    ? await run(
        () =>
          api.patchChannel(props.channel!.id, {
            ...common,
            ...(state.value.input.trim() ? { input: state.value.input.trim() } : {})
          }),
        'Channel saved'
      )
    : await run(
        () =>
          api.createChannel({
            ...common,
            source: resolution!.source,
            input: null,
            playlists: state.value.playlists
          }),
        'Channel added'
      )
  saving.value = false
  if (result.ok) {
    await invalidate()
    open.value = false
  }
}
</script>

<template>
  <USlideover v-model:open="open" :title="editing ? `Edit ${channel?.name}` : 'Add channel'">
    <template #body>
      <form class="flex flex-col gap-4" @submit.prevent="save">
        <UFormField
          :label="editing ? 'Replace source (optional)' : 'Source'"
          :help="editing ? `Current: ${sourceLabel(channel!.source)}` : 'acestream://…, a 40-hex id, a vk.com video link, or any stream URL'"
          :error="resolveError || undefined"
        >
          <UInput
            v-model="state.input"
            class="w-full"
            placeholder="acestream://…"
            autocomplete="off"
            autocapitalize="off"
            @blur="resolveInput"
            @keydown.enter.prevent="resolveInput"
          />
        </UFormField>

        <UAlert
          v-if="currentResolution"
          :color="currentResolution.duplicate_of != null ? 'warning' : 'neutral'"
          variant="subtle"
          :icon="currentResolution.duplicate_of != null ? 'i-lucide-copy' : 'i-lucide-link'"
          :title="currentResolution.duplicate_of != null ? `Already exists as channel ${currentResolution.duplicate_of}` : sourceLabel(currentResolution.source)"
          :description="currentResolution.stream_url"
          :ui="{ description: 'break-all' }"
        />

        <UFormField label="Name" required>
          <UInput v-model="state.name" class="w-full" />
        </UFormField>

        <UFormField label="Group">
          <UInputMenu
            v-model="state.group_title"
            :items="groups"
            create-item
            class="w-full"
            placeholder="Sports"
            @create="(g: string) => (state.group_title = g)"
          />
        </UFormField>

        <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <UFormField label="tvg-id" help="Matches the XMLTV guide channel">
            <UInput v-model="state.tvg_id" class="w-full" autocapitalize="off" />
          </UFormField>
          <UFormField label="Logo URL">
            <UInput v-model="state.tvg_logo" class="w-full" autocapitalize="off">
              <template v-if="state.tvg_logo" #trailing>
                <img :src="state.tvg_logo" alt="" class="size-5 object-contain" />
              </template>
            </UInput>
          </UFormField>
        </div>

        <USwitch v-model="state.is_event" label="Live event" description="A one-off match or stream (e.g. VK)" />
        <UFormField v-if="state.is_event" label="Kickoff" help="Optional, your local time">
          <UInput v-model="state.kickoff" type="datetime-local" class="w-full" />
        </UFormField>

        <USwitch v-model="state.enabled" label="Enabled" description="Disabled channels keep their number but are left out of playlists" />

        <UFormField v-if="!editing" label="Add to playlists">
          <USelectMenu
            v-model="state.playlists"
            :items="playlistItems"
            value-key="value"
            multiple
            class="w-full"
            placeholder="None"
          />
        </UFormField>
        <button type="submit" class="hidden" />
      </form>
    </template>
    <template #footer>
      <div class="flex justify-end gap-2 w-full">
        <UButton color="neutral" variant="ghost" label="Cancel" @click="open = false" />
        <UButton :label="editing ? 'Save' : 'Add'" :loading="saving" :disabled="!canSave" @click="save" />
      </div>
    </template>
  </USlideover>
</template>
