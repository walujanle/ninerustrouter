import assert from "node:assert/strict";
import { test } from "node:test";

import {
	filterProxyPools,
	getVisibleProxyPoolSelection,
	updateProxyPoolSelection,
} from "../src/utils/proxyPoolSelection.ts";

const pools = [
	{ id: "a", isActive: true },
	{ id: "b", isActive: false },
	{ id: "c", isActive: true },
	{ id: "d", isActive: false },
	{ id: "e", isActive: false },
];

test("filters by status without reordering or mutating", () => {
	assert.deepEqual(
		filterProxyPools(pools, "all").map((p) => p.id),
		["a", "b", "c", "d", "e"],
	);
	assert.deepEqual(
		filterProxyPools(pools, "enabled").map((p) => p.id),
		["a", "c"],
	);
	assert.deepEqual(
		filterProxyPools(pools, "disabled").map((p) => p.id),
		["b", "d", "e"],
	);
	assert.deepEqual(
		pools.map((p) => p.id),
		["a", "b", "c", "d", "e"],
	);
});

test("bulk targets drop hidden and deleted pools", () => {
	const visible = filterProxyPools(pools, "disabled");
	assert.deepEqual(
		getVisibleProxyPoolSelection(visible, ["a", "b", "gone", "e"]),
		["b", "e"],
	);
});

test("shift-click selects the visible range in either direction", () => {
	const visible = filterProxyPools(pools, "disabled");
	assert.deepEqual(
		updateProxyPoolSelection(visible, [], {
			id: "e",
			anchorId: "b",
			checked: true,
			shiftKey: true,
		}),
		["b", "d", "e"],
	);
	assert.deepEqual(
		updateProxyPoolSelection(visible, [], {
			id: "b",
			anchorId: "e",
			checked: true,
			shiftKey: true,
		}),
		["b", "d", "e"],
	);
});

test("deselecting a range keeps the selection outside it", () => {
	assert.deepEqual(
		updateProxyPoolSelection(pools, ["a", "b", "c", "d", "e"], {
			id: "b",
			anchorId: "d",
			checked: false,
			shiftKey: true,
		}),
		["a", "e"],
	);
});

test("no shift toggles one pool; a hidden anchor falls back to one", () => {
	assert.deepEqual(
		updateProxyPoolSelection(pools, ["a"], {
			id: "e",
			anchorId: "a",
			checked: true,
			shiftKey: false,
		}),
		["a", "e"],
	);
	assert.deepEqual(
		updateProxyPoolSelection(filterProxyPools(pools, "disabled"), [], {
			id: "e",
			anchorId: "a",
			checked: true,
			shiftKey: true,
		}),
		["e"],
	);
	assert.deepEqual(
		updateProxyPoolSelection(pools, ["a"], {
			id: "gone",
			checked: true,
			shiftKey: true,
		}),
		["a"],
	);
});
