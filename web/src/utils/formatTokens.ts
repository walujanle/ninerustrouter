// Compact token counts for chart axes and tooltips.
//
// The per-chart one-liners stopped at M, so a billion-token Y axis rendered
// "1000.0M" and clipped. Units promote at the rounding boundary (999_950 ->
// "1.0M") so an axis never shows 1000.0 of anything.
const UNITS = [
	{ value: 1e12, suffix: "T" },
	{ value: 1e9, suffix: "B" },
	{ value: 1e6, suffix: "M" },
	{ value: 1e3, suffix: "K" },
];

export function fmtCompactTokens(n?: number | null): string {
	const num = Number(n);
	if (!Number.isFinite(num) || num === 0) return "0";
	const abs = Math.abs(num);
	const sign = num < 0 ? "-" : "";

	for (let i = 0; i < UNITS.length; i++) {
		const { value, suffix } = UNITS[i];
		if (abs >= value) {
			const scaled = abs / value;
			if (Number(scaled.toFixed(1)) >= 1000 && i > 0) {
				const upper = UNITS[i - 1];
				return `${sign}${(abs / upper.value).toFixed(1)}${upper.suffix}`;
			}
			return `${sign}${scaled.toFixed(1)}${suffix}`;
		}
	}

	if (Math.round(abs) >= 1000) return `${sign}1.0K`;
	return String(num);
}
