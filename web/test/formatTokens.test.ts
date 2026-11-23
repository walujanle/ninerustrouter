import assert from "node:assert/strict";
import { test } from "node:test";

import { fmtCompactTokens } from "../src/utils/formatTokens.ts";

test("zero, nullish and small counts stay readable", () => {
	assert.equal(fmtCompactTokens(0), "0");
	assert.equal(fmtCompactTokens(null), "0");
	assert.equal(fmtCompactTokens(undefined), "0");
	assert.equal(fmtCompactTokens(842), "842");
});

test("thousands and millions keep one decimal", () => {
	assert.equal(fmtCompactTokens(1000), "1.0K");
	assert.equal(fmtCompactTokens(15_400), "15.4K");
	assert.equal(fmtCompactTokens(550_000_000), "550.0M");
});

test("billions and trillions do not render a 4-digit M string", () => {
	assert.equal(fmtCompactTokens(1_000_000_000), "1.0B");
	assert.equal(fmtCompactTokens(1_650_000_000), "1.6B");
	assert.equal(fmtCompactTokens(1_250_000_000_000), "1.3T");
});

test("boundary values promote into the next unit", () => {
	assert.equal(fmtCompactTokens(999_950), "1.0M");
	assert.equal(fmtCompactTokens(999_950_000), "1.0B");
	assert.equal(fmtCompactTokens(999_950_000_000), "1.0T");
});
