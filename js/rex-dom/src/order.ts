/**
 * Per-parent ordering: a sorted list of (orderKey, childKey) with binary
 * search, so mounts and moves can find their `insertBefore` reference (the
 * successor) in O(log n). The order key is a fractional-index string; the
 * child key is the tiebreak, making the order total (nesting-draft §5).
 */
export class OrderIndex {
  /** parentKey -> sorted [orderKey, childKey][] */
  private byParent = new Map<string, [string, string][]>();

  private static cmp(a: [string, string], b: [string, string]): number {
    if (a[0] !== b[0]) return a[0] < b[0] ? -1 : 1;
    if (a[1] !== b[1]) return a[1] < b[1] ? -1 : 1;
    return 0;
  }

  /** Index of the first entry >= probe. */
  private static lowerBound(list: [string, string][], probe: [string, string]): number {
    let lo = 0;
    let hi = list.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (OrderIndex.cmp(list[mid]!, probe) < 0) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  }

  insert(parent: string, orderKey: string, child: string): void {
    let list = this.byParent.get(parent);
    if (!list) {
      list = [];
      this.byParent.set(parent, list);
    }
    list.splice(OrderIndex.lowerBound(list, [orderKey, child]), 0, [orderKey, child]);
  }

  remove(parent: string, orderKey: string, child: string): void {
    const list = this.byParent.get(parent);
    if (!list) return;
    const at = OrderIndex.lowerBound(list, [orderKey, child]);
    const hit = list[at];
    if (hit && hit[0] === orderKey && hit[1] === child) list.splice(at, 1);
  }

  /** The child key ordered immediately after (orderKey, child), or null. */
  successor(parent: string, orderKey: string, child: string): string | null {
    const list = this.byParent.get(parent);
    if (!list) return null;
    let at = OrderIndex.lowerBound(list, [orderKey, child]);
    const hit = list[at];
    if (hit && hit[0] === orderKey && hit[1] === child) at++;
    return list[at]?.[1] ?? null;
  }

  /** All children of a parent in order (rebalance and debugging). */
  childrenOf(parent: string): readonly [string, string][] {
    return this.byParent.get(parent) ?? [];
  }
}
