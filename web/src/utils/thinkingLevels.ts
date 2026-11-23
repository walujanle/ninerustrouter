// Thinking-level sets for the model picker.
//
// `/api/models` publishes the authoritative set in `caps.thinkingLevels`
// (derived from `thinkingFormat` and `PATTERN_THINKING` server-side), so the
// client copy below is only a fallback for a row fetched before that field
// existed. Keep the two in step: a level the backend offers and this table
// omits still renders, one this table invents does not.

const BASE = ["none", "low", "medium", "high"];
const CODEX_GPT_5_6_LEVELS = [
	"none",
	"minimal",
	"low",
	"medium",
	"high",
	"xhigh",
	"max",
];

const PATTERN_THINKING: Array<{
	provider?: string;
	pattern: string;
	levels: string[];
}> = [
	{ provider: "codex", pattern: "*gpt-6*", levels: CODEX_GPT_5_6_LEVELS },
	{
		provider: "codex",
		pattern: "*gpt-5.6-sol*",
		levels: [...CODEX_GPT_5_6_LEVELS, "ultra"],
	},
	{
		provider: "codex",
		pattern: "*gpt-5.6-terra*",
		levels: [...CODEX_GPT_5_6_LEVELS, "ultra"],
	},
	{
		provider: "codex",
		pattern: "*gpt-5.6-luna*",
		levels: CODEX_GPT_5_6_LEVELS,
	},
	{ pattern: "*codex*", levels: ["low", "medium", "high", "xhigh"] },
	{
		pattern: "*mimo*v2.6*",
		levels: ["none", "low", "medium", "high", "xhigh"],
	},
	{
		pattern: "*mimo*v2.5-pro*",
		levels: ["none", "low", "medium", "high", "xhigh"],
	},
	{
		pattern: "*deepseek-v4.*",
		levels: ["none", "low", "medium", "high", "xhigh", "max"],
	},
	{
		provider: "codebuddy-intl",
		pattern: "deepseek-v4*",
		levels: ["low", "high", "xhigh"],
	},
];

function matchPattern(pattern: string, model: string): boolean {
	const regex = new RegExp(
		`^${pattern
			.split("*")
			.map((s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"))
			.join(".*")}$`,
		"i",
	);
	return regex.test(model);
}

/** Valid thinking levels for a model, or null when it cannot reason. */
export function getThinkingLevels(
	provider: string,
	model: string,
	caps: { reasoning?: boolean; thinkingLevels?: string[] } | null | undefined,
	modelRows: Array<Record<string, any>> = [],
): string[] | null {
	if (!caps?.reasoning) return null;
	if (caps.thinkingLevels?.length) return caps.thinkingLevels;
	const baseId = String(model || "").replace(/\([^()]+\)\s*$/, "");

	const rowLevels = modelRows.find((entry) => entry.id === baseId)
		?.thinkingLevels as string[] | undefined;
	const hit = PATTERN_THINKING.find(
		(entry) =>
			(!entry.provider || entry.provider === provider) &&
			matchPattern(entry.pattern, model),
	);
	return rowLevels || hit?.levels || BASE;
}
