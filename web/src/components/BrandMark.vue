<script setup lang="ts">
// The NineRustRouter mark: a geometric nine whose bowl is the ingress node and
// whose descender forks into three terminal routes. Drawn inline rather than
// loaded from `assets/logo.svg` so it inherits `currentColor` — the same mark
// reads white on a brand tile and orange on the dark landing pages. Geometry is
// the master's (512 box, stroke 44) verbatim; `web/public/favicon.svg` and the
// two PWA icons carry the same construction with a dark badge behind it.
</script>

<template>
	<svg
		viewBox="0 0 512 512"
		fill="none"
		stroke="currentColor"
		stroke-width="44"
		stroke-linecap="round"
		stroke-linejoin="round"
		aria-hidden="true"
		focusable="false"
	>
		<circle cx="205" cy="164" r="74" />
		<path d="M279 164V292L185.5 370.5M279 292V414M279 292L372.5 370.5" />
		<g fill="currentColor" stroke="none">
			<circle cx="185.5" cy="370.5" r="30" />
			<circle cx="279" cy="414" r="30" />
			<circle cx="372.5" cy="370.5" r="30" />
		</g>
	</svg>
</template>
