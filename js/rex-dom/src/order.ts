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
  return compareSortKeys(sortKey(a), sortKey(b));
}

/** `#<sort>:<seq>` -> [sort, seq] as numbers (NaN-safe: falls back to 0). */
function idParts(s: string): [number, number] {
  const colon = s.indexOf(":");
  return [Number(s.slice(1, colon)) || 0, Number(s.slice(colon + 1)) || 0];
}

/**
 * A canonically-encoded value decoded once for comparison: the order
 * `compareEncoded` defines, without re-parsing the string on every compare
 * (P-6: a binary search over 10k rows used to parse ~30 `BigInt`s per insert).
 */
export interface SortKey {
  readonly raw: string;
  /** The two-character type tag (`i:`, `m:`, `#3`…); numbers compare only
   *  within one tag, as in `compareEncoded`. */
  readonly tag: string;
  /** `i:` / `m:` payload: a number when exactly representable, else a bigint. */
  readonly num?: number | bigint;
  /** Entity id `#sort:seq`. */
  readonly id?: readonly [number, number];
}

export function sortKey(raw: string): SortKey {
  const tag = raw.slice(0, 2);
  if (tag === "i:" || tag === "m:") {
    const body = raw.slice(2);
    // 15 digits always fit a double exactly; longer goes through BigInt.
    if (/^-?\d{1,15}$/.test(body)) return { raw, tag, num: Number(body) };
    try {
      return { raw, tag, num: BigInt(body) };
    } catch {
      return { raw, tag };
    }
  }
  if (raw[0] === "#") return { raw, tag, id: idParts(raw) };
  return { raw, tag };
}

/** `compareEncoded` over decoded keys. */
export function compareSortKeys(a: SortKey, b: SortKey): number {
  if (a.raw === b.raw) return 0;
  if (a.num !== undefined && b.num !== undefined && a.tag === b.tag) {
    return a.num < b.num ? -1 : a.num > b.num ? 1 : 0;
  }
  if (a.id && b.id) {
    if (a.id[0] !== b.id[0]) return a.id[0] < b.id[0] ? -1 : 1;
    if (a.id[1] !== b.id[1]) return a.id[1] < b.id[1] ? -1 : 1;
    return 0;
  }
  return a.raw < b.raw ? -1 : 1;
}

/** One ordered child: `[orderKey, childKey]` plus their decoded forms. */
export type OrderEntry = readonly [orderKey: string, child: string, ok: SortKey, ck: SortKey];

/**
 * Per-parent ordering: a sorted list of (orderKey, childKey) with binary
 * search, so mounts and moves can find their `insertBefore` reference (the
 * successor) in O(log n). The order key is compared by its encoded type
 * (`compareEncoded`), reversed for `desc`; the child key is the ascending
 * tiebreak, making the order total (nesting-draft §5).
 *
 * Keys are decoded once per entry ([`sortKey`]). Bulk changes go through
 * `insertMany` / `removeMany`, one merge or filter pass per parent, so a
 * batch of k changes over n children costs O(n + k log k), not k splices.
 */
export class OrderIndex {
  /** parentKey -> sorted entries */
  private byParent = new Map<string, OrderEntry[]>();

  constructor(private readonly desc = false) {}

  private cmp(a: OrderEntry, b: OrderEntry): number {
    const k = compareSortKeys(a[2], b[2]);
    if (k !== 0) return this.desc ? -k : k;
    return compareSortKeys(a[3], b[3]);
  }

  private static entry(orderKey: string, child: string): OrderEntry {
    return [orderKey, child, sortKey(orderKey), sortKey(child)];
  }

  /** Index of the first entry >= probe. */
  private lowerBound(list: readonly OrderEntry[], probe: OrderEntry): number {
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
    this.insertMany(parent, [[orderKey, child]]);
  }

  /** Insert several children of one parent: sort them, then one merge. */
  insertMany(parent: string, items: readonly (readonly [orderKey: string, child: string])[]): void {
    if (items.length === 0) return;
    const fresh = items.map(([k, c]) => OrderIndex.entry(k, c));
    if (fresh.length > 1) fresh.sort((a, b) => this.cmp(a, b));
    const list = this.byParent.get(parent);
    if (!list || list.length === 0) {
      this.byParent.set(parent, fresh);
      return;
    }
    // Appending past the end (the common bulk case) or a single insert need
    // no merge.
    if (this.cmp(list[list.length - 1]!, fresh[0]!) < 0) {
      for (const e of fresh) list.push(e);
      return;
    }
    if (fresh.length === 1) {
      list.splice(this.lowerBound(list, fresh[0]!), 0, fresh[0]!);
      return;
    }
    const merged: OrderEntry[] = new Array(list.length + fresh.length);
    let i = 0;
    let j = 0;
    let o = 0;
    while (i < list.length && j < fresh.length) {
      merged[o++] = this.cmp(list[i]!, fresh[j]!) <= 0 ? list[i++]! : fresh[j++]!;
    }
    while (i < list.length) merged[o++] = list[i++]!;
    while (j < fresh.length) merged[o++] = fresh[j++]!;
    this.byParent.set(parent, merged);
  }

  remove(parent: string, orderKey: string, child: string): void {
    const list = this.byParent.get(parent);
    if (!list) return;
    const at = this.lowerBound(list, OrderIndex.entry(orderKey, child));
    const hit = list[at];
    if (hit && hit[0] === orderKey && hit[1] === child) list.splice(at, 1);
    if (list.length === 0) this.byParent.delete(parent);
  }

  /** Remove several children of one parent (by child key) in one pass. */
  removeMany(parent: string, children: ReadonlySet<string>): void {
    const list = this.byParent.get(parent);
    if (!list || children.size === 0) return;
    if (children.size === 1) {
      const [child] = children;
      const hit = list.find((e) => e[1] === child);
      if (hit) this.remove(parent, hit[0], child!);
      return;
    }
    const kept = list.filter((e) => !children.has(e[1]));
    if (kept.length === 0) this.byParent.delete(parent);
    else this.byParent.set(parent, kept);
  }

  /** The child key ordered immediately after (orderKey, child), or null. */
  successor(parent: string, orderKey: string, child: string): string | null {
    const list = this.byParent.get(parent);
    if (!list) return null;
    let at = this.lowerBound(list, OrderIndex.entry(orderKey, child));
    const hit = list[at];
    if (hit && hit[0] === orderKey && hit[1] === child) at++;
    return list[at]?.[1] ?? null;
  }

  /** All children of a parent in order (rebalance and debugging). Each entry
   *  starts `[orderKey, childKey, …]`. */
  childrenOf(parent: string): readonly OrderEntry[] {
    return this.byParent.get(parent) ?? [];
  }
}
