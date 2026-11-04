<script setup lang="ts">
import { computed, ref, watch } from "vue";

import Button from "@/components/ui/UiButton.vue";
import Card from "@/components/ui/UiCard.vue";
import Input from "@/components/ui/UiInput.vue";
import { useNotificationStore } from "@/stores/notification";

// Per-provider outbound header overrides. The registry `transport.headers`
// are the baseline; a row whose value differs from it becomes an override, and
// "Restore defaults" sends an empty set, which deletes the entry server-side.
//
// This mirrors the server guard in crates/router-sse/src/providers/overrides.rs.
// The server is the real gate; this only catches an obviously bad submit early.

const MAX_HEADER_VALUE_LENGTH = 8192;
const HEADER_NAME_RE = /^[A-Za-z0-9-]+$/;
const BLOCKED_HEADERS = new Set([
	"host",
	"content-length",
	"content-type",
	"connection",
	"transfer-encoding",
	"authorization",
	"cookie",
]);

const props = defineProps<{ providerId: string }>();

const notify = useNotificationStore();

interface HeaderRow {
	name: string;
	value: string;
	builtin: boolean;
}

const rows = ref<HeaderRow[]>([]);
const builtins = ref<Record<string, string>>({});
const loading = ref(false);
const saving = ref(false);
const error = ref("");

const hasOverride = computed(() => rows.value.some((row) => !row.builtin));

function validate(row: HeaderRow): string {
	if (!HEADER_NAME_RE.test(row.name)) return `Invalid header name: ${row.name}`;
	if (BLOCKED_HEADERS.has(row.name.toLowerCase()))
		return `${row.name} cannot be overridden`;
	if (row.value.length > MAX_HEADER_VALUE_LENGTH) return `${row.name} value is too long`;
	if (/[\r\n]/.test(row.value)) return `${row.name} value contains a newline`;
	return "";
}

/** Rebuild the row list from a `{ headers, builtinHeaders }` response. */
function applyResponse(data: Record<string, any>) {
	const overrides: Record<string, string> = data.headers || {};
	builtins.value = data.builtinHeaders || {};
	const names = [...new Set([...Object.keys(builtins.value), ...Object.keys(overrides)])];
	rows.value = names.map((name) => ({
		name,
		value: name in overrides ? overrides[name] : builtins.value[name],
		builtin: !(name in overrides),
	}));
}

async function load() {
	loading.value = true;
	error.value = "";
	try {
		const res = await fetch(`/api/providers/${encodeURIComponent(props.providerId)}/overrides`, {
			cache: "no-store",
		});
		if (!res.ok) throw new Error("request failed");
		applyResponse(await res.json());
	} catch {
		error.value = "Failed to load custom headers";
	} finally {
		loading.value = false;
	}
}

/** Everything the user typed, minus the built-in rows left untouched. */
function overridesPayload(): Record<string, string> {
	const out: Record<string, string> = {};
	rows.value.forEach((row) => {
		if (!row.name.trim()) return;
		// A built-in whose value is unchanged stays a built-in.
		if (row.builtin && row.value === builtins.value[row.name]) return;
		if (validate(row)) return;
		out[row.name] = row.value;
	});
	return out;
}

async function save() {
	const invalid = rows.value.filter((row) => row.name.trim()).map(validate).find(Boolean);
	if (invalid) {
		error.value = invalid;
		return;
	}
	saving.value = true;
	error.value = "";
	try {
		const res = await fetch(`/api/providers/${encodeURIComponent(props.providerId)}/overrides`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ headers: overridesPayload() }),
		});
		const data = await res.json().catch(() => ({}));
		if (!res.ok) {
			notify.error(data.error || "Failed to save custom headers");
			return;
		}
		// The server answers with the stored override set only; re-read so the
		// built-ins stay the baseline the rows are diffed against.
		await load();
		notify.success("Custom headers saved");
	} catch {
		notify.error("Failed to save custom headers");
	} finally {
		saving.value = false;
	}
}

async function restoreDefaults() {
	saving.value = true;
	error.value = "";
	try {
		const res = await fetch(`/api/providers/${encodeURIComponent(props.providerId)}/overrides`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ headers: {} }),
		});
		if (!res.ok) {
			const data = await res.json().catch(() => ({}));
			notify.error(data.error || "Failed to restore defaults");
			return;
		}
		await load();
		notify.success("Built-in headers restored");
	} catch {
		notify.error("Failed to restore defaults");
	} finally {
		saving.value = false;
	}
}

function addRow() {
	rows.value.push({ name: "", value: "", builtin: false });
}

function removeRow(index: number) {
	rows.value.splice(index, 1);
}

watch(() => props.providerId, load, { immediate: true });
</script>

<template>
  <Card>
    <div class="flex flex-wrap items-center justify-between gap-2">
      <div>
        <h3 class="text-sm font-semibold text-text-main">Custom Headers</h3>
        <p class="text-xs text-text-muted">
          Sent upstream on every request. A value equal to the built-in one is not stored.
        </p>
      </div>
      <div class="flex items-center gap-2">
        <Button
          variant="outline"
          size="sm"
          :disabled="saving || loading || !hasOverride"
          @click="restoreDefaults"
        >
          Restore defaults
        </Button>
        <Button size="sm" :loading="saving" :disabled="loading" @click="save">Save</Button>
      </div>
    </div>

    <p v-if="error" class="mt-2 text-xs text-red-500">{{ error }}</p>
    <p v-else-if="loading" class="mt-2 text-xs text-text-muted">Loading...</p>

    <div v-else class="mt-3 flex flex-col gap-2">
      <div
        v-for="(row, index) in rows"
        :key="`${row.name}-${index}`"
        class="grid grid-cols-1 gap-2 sm:grid-cols-[minmax(0,2fr)_minmax(0,3fr)_auto]"
      >
        <Input
          v-model="row.name"
          placeholder="Header-Name"
          :disabled="row.builtin"
          input-class-name="font-mono text-xs"
        />
        <Input v-model="row.value" placeholder="value" input-class-name="font-mono text-xs" />
        <div class="flex items-center justify-end gap-1">
          <span
            v-if="row.builtin"
            class="rounded bg-black/5 px-1.5 py-0.5 text-[10px] text-text-muted dark:bg-white/5"
          >built-in</span>
          <button
            v-else
            type="button"
            class="rounded p-1 text-text-muted hover:bg-surface-2 hover:text-red-500"
            aria-label="Remove header"
            @click="removeRow(index)"
          >
            <span class="material-symbols-outlined text-[18px]">delete</span>
          </button>
        </div>
      </div>
      <Button variant="ghost" size="sm" icon="add" class="self-start" @click="addRow">
        Add header
      </Button>
    </div>
  </Card>
</template>