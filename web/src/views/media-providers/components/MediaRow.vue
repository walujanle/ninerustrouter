<script setup lang="ts">
// Label + control row used by the example cards.
//
// For a control the label is a real `<label for>` bound to an id handed to the
// slot: a caption next to a control is not a name, and without the association
// a screen reader announces "edit text, blank" for every field in the card. The
// slot must put `:id="id"` on the control.
//
// `readonlyValue` is for a row whose value is text rather than a control (the resolved
// endpoint, a masked key). There is nothing to name, so the caption stays a
// `<span>` and no id is generated.
import { useId } from "vue";

defineProps<{ label: string; readonlyValue?: boolean }>();
const id = useId();
</script>

<template>
  <div class="flex min-w-0 flex-col gap-1.5 sm:flex-row sm:items-center sm:gap-3">
    <span v-if="readonlyValue" class="w-full text-xs font-medium text-text-muted sm:w-20 sm:shrink-0">{{ label }}</span>
    <label v-else :for="id" class="w-full text-xs font-medium text-text-muted sm:w-20 sm:shrink-0">{{ label }}</label>
    <div class="w-full min-w-0 flex-1">
      <slot :id="readonlyValue ? undefined : id" />
    </div>
  </div>
</template>
