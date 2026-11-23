// Proxy-pool row filtering and bulk selection.
//
// Bulk actions must never touch a row hidden by the status filter, and a
// shift-click selects the visible range only, so these rules live apart from
// the page and are unit-tested directly.
type PoolId = string | number;

interface Pool {
	id: PoolId;
	isActive?: boolean;
}

export function filterProxyPools<T extends Pool>(
	pools: T[],
	status: string,
): T[] {
	if (status === "enabled")
		return pools.filter((pool) => pool.isActive === true);
	if (status === "disabled")
		return pools.filter((pool) => pool.isActive !== true);
	return pools;
}

export function getVisibleProxyPoolSelection(
	pools: Pool[],
	selectedIds: PoolId[],
): PoolId[] {
	const selected = new Set(selectedIds);
	return pools.filter((pool) => selected.has(pool.id)).map((pool) => pool.id);
}

export function updateProxyPoolSelection(
	pools: Pool[],
	selectedIds: PoolId[],
	{
		id,
		anchorId,
		checked,
		shiftKey,
	}: {
		id: PoolId;
		anchorId?: PoolId | null;
		checked: boolean;
		shiftKey?: boolean;
	},
): PoolId[] {
	const ids = pools.map((pool) => pool.id);
	const selected = new Set(getVisibleProxyPoolSelection(pools, selectedIds));
	const index = ids.indexOf(id);
	if (index === -1) return [...selected];
	const anchor = shiftKey && anchorId != null ? ids.indexOf(anchorId) : -1;
	const range =
		anchor === -1
			? [id]
			: ids.slice(Math.min(anchor, index), Math.max(anchor, index) + 1);
	for (const target of range) {
		if (checked) selected.add(target);
		else selected.delete(target);
	}
	return ids.filter((target) => selected.has(target));
}
