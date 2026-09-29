/**
 * Compare two canonically-encoded values by their *type's* order, not by their
 * spelling (S-70): `i:3` sorts before `i:10`, and entity id `#3:9` before
 * `#3:10`. The encoding is self-describing (`rex::eval::encode`), so the tag
 * on the value picks the comparator; values of different types (which one
 * order view never mixes) fall back to plain string order so the result is
 * still total.
 */
export function compareEncoded(a: string, b: string): number {
  if (a === b) return 0;
  const num = (s: string): bigint | undefined => {
    if (s.startsWith("i:") || s.startsWith("m:")) {
      try {
        return BigInt(s.slice(2));
      } catch {
        return undefined;
      }
    }
    return undefined;
  };
  if (a[0] === b[0] && a[1] === b[1]) {
    const x = num(a);
    const y = num(b);
    if (x !== undefined && y !== undefined) return x < y ? -1 : x > y ? 1 : 0;
  }
  if (a[0] === "#" && b[0] === "#") {
    const [sa, na] = idParts(a);
    const [sb, nb] = idParts(b);
    if (sa !== sb) return sa < sb ? -1 : 1;
    if (na !== nb) return na < nb ? -1 : 1;
    return 0;
  }
  return a < b ? -1 : 1;
}

/** `#<sort>:<seq>` -> [sort, seq] as numbers (NaN-safe: falls back to 0). */
function idParts(s: string): [number, number] {
  const colon = s.indexOf(":");
  return [Number(s.slice(1, colon)) || 0, Number(s.slice(colon + 1)) || 0];
}

/**
 * Per-parent ordering: a sorted list of (orderKey, childKey) with binary
 * search, so mounts and moves can find their `insertBefore` reference (the
 * successor) in O(log n). The order key is compared by its encoded type
 * (`compareEncoded`), reversed for `desc`; the child key is the ascending
 * tiebreak, making the order total (nesting-draft §5).
 */
export class OrderIndex {
  /** parentKey -> sorted [orderKey, childKey][] */
  private byParent = new Map<string, [string, string][]>();

  constructor(private readonly desc = false) {}

  private cmp(a: [string, string], b: [string, string]): number {
    const k = compareEncoded(a[0], b[0]);
    if (k !== 0) return this.desc ? -k : k;
    return compareEncoded(a[1], b[1]);
  }

  /** Index of the first entry >= probe. */
  private lowerBound(list: [string, string][], probe: [string, string]): number {
    let lo = 0;
    let hi = list.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (this.cmp(list[mid]!, probe) < 0) lo = mid + 1;
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
    list.splice(this.lowerBound(list, [orderKey, child]), 0, [orderKey, child]);
  }

  remove(parent: string, orderKey: string, child: string): void {
    const list = this.byParent.get(parent);
    if (!list) return;
    const at = this.lowerBound(list, [orderKey, child]);
    const hit = list[at];
    if (hit && hit[0] === orderKey && hit[1] === child) list.splice(at, 1);
  }

  /** The child key ordered immediately after (orderKey, child), or null. */
  successor(parent: string, orderKey: string, child: string): string | null {
    const list = this.byParent.get(parent);
    if (!list) return null;
    let at = this.lowerBound(list, [orderKey, child]);
    const hit = list[at];
    if (hit && hit[0] === orderKey && hit[1] === child) at++;
    return list[at]?.[1] ?? null;
  }

  /** All children of a parent in order (rebalance and debugging). */
  childrenOf(parent: string): readonly [string, string][] {
    return this.byParent.get(parent) ?? [];
  }
}
