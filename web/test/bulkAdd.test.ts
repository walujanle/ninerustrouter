import assert from "node:assert/strict";
import { test } from "node:test";

import { planBulkAdd } from "../src/utils/bulkAdd.ts";

test("name|key lines keep the name and the whole rest as the key", () => {
	const plan = planBulkAdd(["work|sk-abc|def"], []);
	assert.equal(plan.length, 1);
	assert.equal(plan[0].name, "work 1");
	assert.equal(plan[0].apiKey, "sk-abc|def");
});

test("a bare key is auto-named Key N", () => {
	const plan = planBulkAdd(["sk-only"], []);
	assert.equal(plan[0].name, "Key 1");
	assert.equal(plan[0].apiKey, "sk-only");
});

test("an LLM|id|secret token stays opaque", () => {
	const plan = planBulkAdd(["LLM|abc123|secret456"], []);
	assert.equal(plan.length, 1);
	assert.equal(plan[0].name, "LLM 1");
	assert.equal(plan[0].apiKey, "LLM|abc123|secret456");
});

test("names gap-fill around existing connections", () => {
	const plan = planBulkAdd(["work|k1", "work|k2"], ["work 1", "work 3"]);
	assert.deepEqual(
		plan.map((p) => p.name),
		["work 2", "work 4"],
	);
});
