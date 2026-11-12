<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { api, routeSummary, sourceLabel, type Channel, type EpgChannel, type ResolveResponse } from '../api'
import EpgSearch from './EpgSearch.vue'
import { lastRoute, rememberRoute, useAction, useChannels, useInvalidateAll, usePlaylists, useRoutes } from '../composables'
import { floating } from '../safe-area'

const props = defineProps<{ channel?: Channel | null }>()
const open = defineModel<boolean>('open', { default: false })

const run = useAction()
const invalidate = useInvalidateAll()
const { data: channels } = useChannels()
const { data: playlists } = usePlaylists()
const { data: routes } = useRoutes()

const editing = computed(() => !!props.channel)

const state = ref({
  route_id: undefined as number | undefined,
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
/** The resolution and the route and input it belongs to; only valid while both are unchanged. */
const resolved = ref<ResolveResponse | null>(null)
const resolvedFor = ref('')
const resolveError = ref('')
const saving = ref(false)
const pickerOpen = ref(false)

/**
 * Values last filled in from a suggestion. A field still holding its filled-in value (or empty)
 * follows the next resolution, including being cleared when that one suggests nothing; a field
 * the user edited is left alone.
 */
const autofilled = ref({ name: '', group_title: '', tvg_id: '', tvg_logo: '' })
type AutoField = keyof typeof autofilled.value
/** The event flag as last set from a suggestion; null until a suggestion set it. */
const autoEvent = ref<boolean | null>(null)

function fill(field: AutoField, value: string | null | undefined) {
  const current = (state.value[field] ?? '') as string
  const owned = current === '' || current === autofilled.value[field]
  if (!owned) return
  state.value[field] = value ?? ''
  autofilled.value[field] = value ?? ''
}

function fillEvent(value: boolean) {
  // Untouched: still the initial value, or what the previous suggestion set.
  const owned = autoEvent.value === null ? !state.value.is_event : state.value.is_event === autoEvent.value
  if (!owned) return
  state.value.is_event = value
  autoEvent.value = value
}

function useMatch(m: EpgChannel) {
  state.value.tvg_id = m.xmltv_id
  if (m.icon) state.value.tvg_logo = m.icon
  pickerOpen.value = false
}

const groups = computed(() =>
  [...new Set((channels.value ?? []).map((c) => c.group_title).filter((g): g is string => !!g))].sort()
)
const playlistItems = computed(() => (playlists.value ?? []).map((p) => ({ label: p.name, value: p.slug })))
const routeItems = computed(() =>
  (routes.value ?? []).map((r) => ({
    label: r.enabled ? r.name : `${r.name} (disabled)`,
    description: routeSummary(r.config),
    value: r.id
  }))
)
const route = computed(() => routes.value?.find((r) => r.id === state.value.route_id))
/** What the selected route accepts, for the input's help text. */
const inputHelp = computed(() => {
  const r = route.value
  if (!r) return 'Pick a route first'
  return r.config.kind === 'direct'
    ? 'An http(s) stream URL, played as it is'
    : `Whatever the gateway's ${r.config.rule} rule accepts`
})
/** Key of what a resolution was made for: route and input. */
const key = (routeId: number | undefined, input: string) => `${routeId ?? ''} ${input}`

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
    const remembered = lastRoute()
    const fallback = routes.value?.find((r) => r.id === remembered) ?? routes.value?.[0]
    state.value.route_id = c?.route_id ?? fallback?.id
    state.value.input = ''
    state.value.name = c?.name ?? ''
    state.value.group_title = c?.group_title ?? undefined
    state.value.tvg_id = c?.tvg_id ?? ''
    state.value.tvg_logo = c?.tvg_logo ?? ''
    state.value.is_event = c?.is_event ?? false
    state.value.kickoff = toLocalInput(c?.kickoff_at)
    state.value.enabled = c?.enabled ?? true
    state.value.playlists = playlists.value?.length === 1 && !c ? [playlists.value[0]!.slug] : []
    autofilled.value = { name: '', group_title: '', tvg_id: '', tvg_logo: '' }
    autoEvent.value = null
  },
  { immediate: true }
)

// Routes may load after the form opened: pick one then.
watch(routes, (list) => {
  if (open.value && state.value.route_id == null && list?.length) {
    state.value.route_id = (list.find((r) => r.id === lastRoute()) ?? list[0])!.id
  }
})

const currentKey = computed(() => key(state.value.route_id, state.value.input.trim()))
watch(currentKey, (k) => {
  if (k !== resolvedFor.value && k !== pendingFor) resetResolution()
})
// Changing the route revalidates a typed input with the new route.
watch(
  () => state.value.route_id,
  () => {
    if (state.value.input.trim()) resolveInput()
  }
)

async function resolveInput() {
  const input = state.value.input.trim()
  const routeId = state.value.route_id
  const k = key(routeId, input)
  if (input && k === resolvedFor.value) return
  resetResolution()
  if (!input || routeId == null) return
  pendingFor = k
  const seq = resolveSeq
  try {
    const r = await api.resolve(routeId, input)
    if (seq !== resolveSeq || !open.value || currentKey.value !== k) return
    pendingFor = ''
    resolved.value = r
    resolvedFor.value = k
    if (!editing.value) {
      const s = r.suggestion
      fill('name', s.name)
      fill('group_title', s.group_title)
      fill('tvg_id', s.tvg_id)
      fill('tvg_logo', s.tvg_logo)
      fillEvent(s.is_event)
    }
  } catch (e) {
    if (seq === resolveSeq) resolveError.value = (e as Error).message
  }
}

/** A resolution that matches the current route and input exactly, or null. */
const currentResolution = computed(() =>
  resolved.value && resolvedFor.value === currentKey.value ? resolved.value : null
)
/** The duplicate, unless it is the channel being edited (its own source again). */
const duplicateOf = computed(() => {
  const d = currentResolution.value?.duplicate_of ?? null
  return editing.value && d === props.channel?.id ? null : d
})
const routeChanged = computed(() => editing.value && state.value.route_id !== props.channel?.route_id)

const canSave = computed(
  () =>
    state.value.name.trim().length > 0 &&
    state.value.route_id != null &&
    (editing.value && !state.value.input.trim()
      ? true
      : !!currentResolution.value && duplicateOf.value == null) &&
    !saving.value
)

/** Point the existing duplicate at this input (e.g. a newer access token in the link). */
async function updateDuplicate() {
  const r = currentResolution.value
  if (!r || r.duplicate_of == null) return
  const result = await run(
    () => api.patchChannel(r.duplicate_of!, { input: state.value.input.trim() }),
    `Channel ${r.duplicate_of} updated`
  )
  if (result.ok) {
    await invalidate()
    open.value = false
  }
}

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
  const input = state.value.input.trim()
  const result = props.channel
    ? await run(
        () =>
          api.patchChannel(props.channel!.id, {
            ...common,
            ...(input ? { input } : {}),
            ...(routeChanged.value ? { route_id: state.value.route_id! } : {})
          }),
        'Channel saved'
      )
    : await run(
        () =>
          api.createChannel({
            ...common,
            route_id: resolution!.route_id,
            input,
            playlists: state.value.playlists
          }),
        'Channel added'
      )
  if (result.ok && !props.channel) rememberRoute(resolution!.route_id)
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
          label="Route"
          :help="routeChanged ? 'The current source is revalidated with the new route' : 'How the channel is played'"
          required
        >
          <USelectMenu
            v-model="state.route_id"
            :content="floating()"
            :items="routeItems"
            value-key="value"
            class="w-full"
            placeholder="Pick a route"
          />
        </UFormField>

        <UFormField
          :label="editing ? 'Replace source (optional)' : 'Source'"
          :help="editing ? `Current: ${channel!.source}` : inputHelp"
          :error="resolveError || undefined"
        >
          <UInput
            v-model="state.input"
            class="w-full"
            :disabled="state.route_id == null"
            placeholder="Paste a link or id"
            autocomplete="off"
            autocapitalize="off"
            @blur="resolveInput"
            @keydown.enter.prevent="resolveInput"
          />
        </UFormField>

        <UAlert
          v-if="currentResolution"
          :color="duplicateOf != null ? 'warning' : 'neutral'"
          variant="subtle"
          :icon="duplicateOf != null ? 'i-lucide-copy' : 'i-lucide-link'"
          :title="
            duplicateOf != null
              ? `Already exists as channel ${duplicateOf}`
              : sourceLabel({ route_name: route?.name ?? '', identity: currentResolution.identity })
          "
          :ui="{ description: 'break-all' }"
        >
          <template #description>
            <div class="break-all">{{ currentResolution.play_url }}</div>
            <div v-if="currentResolution.source !== state.input.trim()" class="mt-1 text-muted">
              Stored as {{ currentResolution.source }}
            </div>
            <div v-if="route && !route.enabled" class="mt-1">
              The route is disabled: the channel stays out of playlists until it is enabled.
            </div>
            <div v-if="duplicateOf != null && currentResolution.duplicate_source" class="mt-1">
              That channel has {{ currentResolution.duplicate_source }}.
              <UButton size="xs" variant="subtle" class="ml-1" label="Update it to this link" @click="updateDuplicate" />
            </div>
            <div v-for="w in currentResolution.warnings ?? []" :key="w" class="mt-1 text-warning">{{ w }}</div>
          </template>
        </UAlert>

        <UFormField label="Name" required>
          <UInput v-model="state.name" class="w-full" />
        </UFormField>

        <UFormField label="Group">
          <UInputMenu
            v-model="state.group_title"
            :content="floating()"
            :items="groups"
            create-item
            class="w-full"
            placeholder="Sports"
            @create="(g: string) => (state.group_title = g)"
          />
        </UFormField>

        <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <UFormField label="tvg-id" help="Matches the XMLTV guide channel">
            <UInput v-model="state.tvg_id" class="w-full" autocapitalize="off">
              <template #trailing>
                <UButton
                  icon="i-lucide-search"
                  color="neutral"
                  variant="link"
                  size="sm"
                  aria-label="Search the guide"
                  @click="pickerOpen = true"
                />
              </template>
            </UInput>
          </UFormField>
          <UFormField label="Logo URL">
            <UInput v-model="state.tvg_logo" class="w-full" autocapitalize="off">
              <template v-if="state.tvg_logo" #trailing>
                <img :src="state.tvg_logo" alt="" class="size-5 object-contain" />
              </template>
            </UInput>
          </UFormField>
        </div>

        <div v-if="currentResolution?.matches?.length && !editing" class="flex flex-wrap items-center gap-1 -mt-2">
          <span class="text-xs text-muted mr-1">Guide matches:</span>
          <UButton
            v-for="m in currentResolution.matches ?? []"
            :key="m.xmltv_id"
            size="xs"
            :color="m.xmltv_id === state.tvg_id ? 'primary' : 'neutral'"
            variant="subtle"
            :label="`${m.xmltv_id} · ${(m.score ?? 0).toFixed(2)}`"
            @click="useMatch(m)"
          />
        </div>

        <USwitch v-model="state.is_event" label="Live event" description="A one-off match or stream" />
        <UFormField v-if="state.is_event" label="Kickoff" help="Optional, your local time">
          <UInput v-model="state.kickoff" type="datetime-local" class="w-full" />
        </UFormField>

        <USwitch v-model="state.enabled" label="Enabled" description="Disabled channels keep their number but are left out of playlists" />

        <UFormField v-if="!editing" label="Add to playlists">
          <USelectMenu
            v-model="state.playlists"
            :content="floating()"
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

  <UModal v-model:open="pickerOpen" title="Find the guide channel">
    <template #body>
      <EpgSearch
        :initial="state.tvg_id || state.name"
        action-label="Use this channel"
        action-icon="i-lucide-check"
        @select="useMatch"
      />
    </template>
  </UModal>
</template>
