<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { api, type Route, type RouteConfig, type RouteTestResult, type RuleSummary } from '../api'
import RouteCard from '../components/RouteCard.vue'
import { useAction, useInvalidateAll, useRoutes } from '../composables'
import { baseUrlError } from '../components/settings/section'
import { floating } from '../safe-area'

const run = useAction()
const invalidate = useInvalidateAll()
const { data: routes, error, refetch } = useRoutes()

/** Example address (kept out of the template: vue-tsc mangles "//" in some attribute values). */
const example = 'http://media-host:8099'

// ---------------------------------------------------------------- add / edit

const formOpen = ref(false)
/** The route being edited, or null when adding. */
const editing = ref<Route | null>(null)
const form = ref({ name: '', kind: 'gateway' as 'gateway' | 'direct', base_url: '', rule: undefined as string | undefined, input: '' })
const saving = ref(false)

/** Rules of the gateway at the typed base URL, and the URL they belong to. */
const rules = ref<RuleSummary[]>([])
const rulesFor = ref('')
const rulesError = ref('')
const loadingRules = ref(false)
const testResult = ref<RouteTestResult | null>(null)
const testing = ref(false)

function openAdd() {
  editing.value = null
  form.value = { name: '', kind: 'gateway', base_url: '', rule: undefined, input: '' }
  resetGateway()
  formOpen.value = true
}

function openEdit(r: Route) {
  editing.value = r
  form.value = {
    name: r.name,
    kind: r.config.kind,
    base_url: r.config.kind === 'gateway' ? r.config.base_url : '',
    rule: r.config.kind === 'gateway' ? r.config.rule : undefined,
    input: ''
  }
  resetGateway()
  formOpen.value = true
}

function resetGateway() {
  rules.value = []
  rulesFor.value = ''
  rulesError.value = ''
  testResult.value = null
}

const base = computed(() => form.value.base_url.trim().replace(/\/+$/, ''))
const baseError = computed(() => (form.value.kind === 'gateway' ? baseUrlError(form.value.base_url) : null))
watch(base, () => {
  testResult.value = null
  if (base.value !== rulesFor.value) {
    rules.value = []
    rulesError.value = ''
  }
})
watch(() => [form.value.rule, form.value.input, form.value.kind], () => (testResult.value = null))

/** Ask the gateway at the typed URL for its rules (a test with no rule lists them). */
async function loadRules() {
  if (baseError.value) return
  const url = base.value
  loadingRules.value = true
  rulesError.value = ''
  try {
    const r = await api.testRoute({ kind: 'gateway', base_url: url, rule: form.value.rule || '-' })
    if (url !== base.value) return
    rulesFor.value = url
    rules.value = r.gateway?.rules ?? []
    if (!r.gateway) rulesError.value = r.detail
    else if (!form.value.rule && rules.value.length === 1) form.value.rule = rules.value[0]!.name
  } catch (e) {
    rulesError.value = (e as Error).message
  } finally {
    loadingRules.value = false
  }
}

const ruleItems = computed(() =>
  rules.value.map((r) => ({ label: r.title, description: r.input_hint ?? r.name, value: r.name }))
)

const draftConfig = computed<RouteConfig>(() =>
  form.value.kind === 'direct'
    ? { kind: 'direct' }
    : { kind: 'gateway', base_url: base.value, rule: form.value.rule ?? '' }
)

async function test() {
  testing.value = true
  const r = await run(() => api.testRoute(draftConfig.value, form.value.input.trim() || undefined))
  testing.value = false
  if (r.ok) testResult.value = r.value
}

const canSave = computed(() => {
  if (!form.value.name.trim() || saving.value) return false
  if (editing.value) return form.value.kind === 'direct' || !baseError.value
  return form.value.kind === 'direct' || (!baseError.value && !!form.value.rule)
})

async function save() {
  if (!canSave.value) return
  saving.value = true
  const e = editing.value
  const result = e
    ? await run(
        () =>
          api.patchRoute(e.id, {
            name: form.value.name.trim(),
            ...(e.config.kind === 'gateway' && base.value !== e.config.base_url ? { base_url: base.value } : {})
          }),
        'Route saved'
      )
    : await run(
        () => api.createRoute({ name: form.value.name.trim(), config: draftConfig.value, enabled: true }),
        'Route added'
      )
  saving.value = false
  if (result.ok) {
    await invalidate()
    formOpen.value = false
  }
}

// ---------------------------------------------------------------- delete

const deleting = ref<Route | null>(null)
async function confirmDelete() {
  const r = deleting.value
  if (!r) return
  deleting.value = null
  if ((await run(() => api.deleteRoute(r.id), `Deleted ${r.name}`)).ok) await invalidate()
}
</script>

<template>
  <UDashboardPanel id="routes">
    <template #header>
      <UDashboardNavbar title="Routes">
        <template #leading>
          <UDashboardSidebarCollapse />
        </template>
        <template #right>
          <UButton icon="i-lucide-plus" label="Add route" @click="openAdd" />
        </template>
      </UDashboardNavbar>
    </template>

    <template #body>
      <div class="flex flex-col gap-4 max-w-2xl w-full">
        <UAlert
          v-if="error && !routes"
          color="error"
          variant="subtle"
          icon="i-lucide-circle-x"
          :title="`Could not load routes: ${error.message}`"
          :actions="[{ label: 'Retry', onClick: () => refetch() }]"
        />
        <p class="text-sm text-muted">
          A route is how channels are played. A direct route puts stream URLs in playlists as they are. A gateway route
          sends links or ids (an Acestream id, a video page) to a gateway, which turns them into playable URLs; players
          then fetch from the gateway or from the service it points at, so use an address your players can reach, not
          localhost. You pick the route when you add a channel.
        </p>
        <RouteCard v-for="r in routes ?? []" :key="r.id" :route="r" @edit="openEdit" @delete="(x) => (deleting = x)" />
      </div>
    </template>
  </UDashboardPanel>

  <USlideover v-model:open="formOpen" :title="editing ? `Edit ${editing.name}` : 'Add route'">
    <template #body>
      <form class="flex flex-col gap-4" @submit.prevent="save">
        <UFormField label="Name" required>
          <UInput v-model="form.name" class="w-full" placeholder="Acestream" />
        </UFormField>

        <UFormField
          v-if="!editing"
          label="Kind"
          help="Can't be changed later: create another route instead"
        >
          <URadioGroup
            v-model="form.kind"
            :items="[
              { label: 'Gateway', description: 'Links or ids a gateway turns into playable URLs', value: 'gateway' },
              { label: 'Direct', description: 'Stream URLs, played as they are', value: 'direct' }
            ]"
          />
        </UFormField>

        <template v-if="form.kind === 'gateway'">
          <UFormField
            label="Gateway URL"
            :help="
              editing
                ? 'Every channel of the route is revalidated with the new address before it is saved; if any fails, nothing changes.'
                : 'As players reach it'
            "
            :error="(form.base_url.trim() && baseError) || undefined"
          >
            <div class="flex flex-col sm:flex-row gap-2">
              <UInput
                v-model="form.base_url"
                type="url"
                inputmode="url"
                autocapitalize="off"
                autocorrect="off"
                spellcheck="false"
                :placeholder="example"
                class="flex-1 min-w-0"
              />
              <UButton
                v-if="!editing"
                icon="i-lucide-plug-zap"
                color="neutral"
                variant="subtle"
                label="Load rules"
                :loading="loadingRules"
                :disabled="!!baseError"
                @click="loadRules"
              />
            </div>
          </UFormField>
          <p v-if="rulesError" class="text-sm text-error -mt-2">{{ rulesError }}</p>

          <UFormField v-if="editing" label="Rule" help="Can't be changed later">
            <UInput :model-value="form.rule ?? ''" disabled class="w-full font-mono" />
          </UFormField>
          <UFormField v-else label="Rule" required :help="rules.length ? 'What the route accepts' : 'Load the gateway\'s rules first'">
            <USelectMenu
              v-model="form.rule"
              :content="floating()"
              :items="ruleItems"
              value-key="value"
              class="w-full"
              :disabled="!rules.length"
              placeholder="Pick a rule"
            />
          </UFormField>
        </template>

        <UFormField label="Try an input (optional)" help="Validates it with this config; nothing is saved">
          <div class="flex flex-col sm:flex-row gap-2">
            <UInput v-model="form.input" class="flex-1 min-w-0" autocapitalize="off" placeholder="Paste a link or id" />
            <UButton
              icon="i-lucide-plug-zap"
              color="neutral"
              variant="subtle"
              label="Test"
              :loading="testing"
              :disabled="form.kind === 'gateway' && (!!baseError || !form.rule)"
              @click="test"
            />
          </div>
        </UFormField>
        <UAlert
          v-if="testResult"
          :color="testResult.ok ? 'success' : 'error'"
          variant="subtle"
          :icon="testResult.ok ? 'i-lucide-circle-check' : 'i-lucide-circle-x'"
          :title="testResult.detail"
          :ui="{ description: 'break-all' }"
        >
          <template v-if="testResult.sample" #description>
            <div>Plays from {{ testResult.sample.play_url }}</div>
            <div class="text-muted">Stored as {{ testResult.sample.source }}</div>
            <div v-if="testResult.sample.hints.title">Title: {{ testResult.sample.hints.title }}</div>
            <div v-for="w in testResult.sample.warnings" :key="w" class="text-warning">{{ w }}</div>
          </template>
        </UAlert>
        <button type="submit" class="hidden" />
      </form>
    </template>
    <template #footer>
      <div class="flex justify-end gap-2 w-full">
        <UButton color="neutral" variant="ghost" label="Cancel" @click="formOpen = false" />
        <UButton :label="editing ? 'Save' : 'Add'" :loading="saving" :disabled="!canSave" @click="save" />
      </div>
    </template>
  </USlideover>

  <UModal
    :open="!!deleting"
    :title="`Delete ${deleting?.name}?`"
    description="The route is removed. Only routes no channel uses can be deleted."
    @update:open="(o: boolean) => !o && (deleting = null)"
  >
    <template #footer>
      <div class="flex justify-end gap-2 w-full">
        <UButton color="neutral" variant="ghost" label="Cancel" @click="deleting = null" />
        <UButton color="error" label="Delete" @click="confirmDelete" />
      </div>
    </template>
  </UModal>
</template>
