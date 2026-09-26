import type { DomDriver, ShapeNode, StepDeltas, Tuple } from "./types.js";
import { OrderIndex } from "./order.js";

/**
 * The shaper: turns one atomic delta batch into one atomic DOM transaction.
 *
 * The #1 rule (nesting-draft §4): a −old/+new pair at the same child key is
 * ONE op — update / move / reparent — never remove+mount. That preserves DOM
 * node identity (focus, selection, scroll, in-flight animation), and getting
 * it wrong is silent. Classification is by *view role* (membership / order /
 * attribute), which the shape tree declares — never by inspecting values.
 *
 * Phases per batch, in this order (nesting-draft §4):
 *   1. removes  — subtree-coalesced (one removeChild per dead subtree root)
 *   2. mounts   — depth ascending; wide row assembled from the mirrors
 *   3. updates  — field-level; keys mounted this batch are skipped
 *   4. moves / reparents — against final membership, via the order index
 */
export class Shaper<El> {
  private readonly driver: DomDriver<El>;
  private readonly container: El;
  /** Flattened shape tree, depth ascending. */
  private readonly levels: Level<El>[] = [];
  private readonly levelByName = new Map<string, Level<El>>();
  /** view name -> integrated Z-set rows (key -> value -> weight). */
  private readonly mirror = new Map<string, Map<string, Map<string, number>>>();
  /** Views that participate in any mirror (everything the shape tree names). */
  private readonly mirroredViews = new Set<string>();

  constructor(driver: DomDriver<El>, container: El, shapes: readonly ShapeNode<El>[]) {
    this.driver = driver;
    this.container = container;
    const walk = (shape: ShapeNode<El>, parent: Level<El> | null, depth: number) => {
      const level: Level<El> = {
        shape,
        parent,
        depth,
        parentOf: new Map(),
        orderOf: new Map(),
        nodes: new Map(),
        order: new OrderIndex(),
      };
      this.levels.push(level);
      if (this.levelByName.has(shape.name)) {
        throw new Error(`duplicate shape name: ${shape.name}`);
      }
      this.levelByName.set(shape.name, level);
      this.mirroredViews.add(shape.membershipView);
      if (shape.orderView) this.mirroredViews.add(shape.orderView);
      for (const a of shape.attrs) this.mirroredViews.add(a.view);
      for (const child of shape.children) walk(child, level, depth + 1);
    };
    for (const s of shapes) walk(s, null, 0);
    this.levels.sort((a, b) => a.depth - b.depth);
  }

  /** The mounted element for a child of a shape level (tests, event wiring). */
  el(shapeName: string, childKey: string): El | undefined {
    return this.levelByName.get(shapeName)?.nodes.get(childKey);
  }

  /** A parent's children in order, as (orderKey, childKey) — what an app
   *  needs to compute a drop position's fractional key, and what the
   *  rebalance helper consumes. */
  orderedChildren(shapeName: string, parentKey: string): readonly [string, string][] {
    return this.levelByName.get(shapeName)?.order.childrenOf(parentKey) ?? [];
  }

  /** Apply one engine step: one delta batch, one synchronous DOM transaction. */
  applyStep(deltas: StepDeltas): void {
    // Phase 0a: integrate every mirrored view's delta, recording per-view
    // touched keys for attribute-update detection.
    const touchedAttrs = new Map<string, Set<string>>(); // view -> keys
    for (const [view, rows] of Object.entries(deltas)) {
      if (!this.mirroredViews.has(view) || rows.length === 0) continue;
      const m = this.mirrorFor(view);
      const touched = new Set<string>();
      for (const [key, value, w] of rows) {
        let byValue = m.get(key);
        if (!byValue) {
          byValue = new Map();
          m.set(key, byValue);
        }
        const next = (byValue.get(value) ?? 0) + w;
        if (next === 0) byValue.delete(value);
        else byValue.set(value, next);
        if (byValue.size === 0) m.delete(key);
        touched.add(key);
      }
      touchedAttrs.set(view, touched);
    }

    // Phase 0b: classify structural changes per level from membership deltas.
    const plans = new Map<Level<El>, LevelPlan>();
    for (const level of this.levels) {
      const rows = deltas[level.shape.membershipView];
      const plan: LevelPlan = { mounts: new Map(), removes: new Set(), reparents: new Map() };
      plans.set(level, plan);
      if (!rows || rows.length === 0) continue;
      const touched = new Set<string>();
      for (const [child] of rows) touched.add(child);
      for (const child of touched) {
        const oldParent = level.parentOf.get(child);
        // Root levels ignore the membership view's value column: presence is
        // what matters, and every root child shares the container ("").
        let newParent = this.resolveOne(level.shape.membershipView, child);
        if (!level.parent && newParent !== undefined) newParent = "";
        if (oldParent === undefined && newParent !== undefined) {
          plan.mounts.set(child, newParent);
        } else if (oldParent !== undefined && newParent === undefined) {
          plan.removes.add(child);
        } else if (
          oldParent !== undefined &&
          newParent !== undefined &&
          oldParent !== newParent
        ) {
          // The −/+ fusion case, structural flavor: same child, new parent.
          plan.reparents.set(child, { from: oldParent, to: newParent });
        }
      }
    }

    // Phase 1: removes, top-down, subtree-coalesced. A removed child whose
    // ancestor is also being removed this batch cleans up its bookkeeping but
    // issues no DOM call — the ancestor's removeChild takes the subtree.
    for (const level of this.levels) {
      const plan = plans.get(level)!;
      for (const child of plan.removes) {
        const el = level.nodes.get(child);
        const parentKey = level.parentOf.get(child)!;
        level.nodes.delete(child);
        level.parentOf.delete(child);
        level.order.remove(parentKey, level.orderOf.get(child) ?? "", child);
        level.orderOf.delete(child);
        if (el !== undefined && !this.ancestorRemoved(level, parentKey, plans)) {
          this.driver.removeChild(this.parentEl(level, parentKey), el);
        }
      }
    }

    // Phase 2: mounts, depth ascending (this.levels is depth-sorted), wide
    // row assembled from the mirrors. Mounted-this-batch keys are recorded so
    // phase 3 skips their attribute deltas.
    const mountedNow = new Map<Level<El>, Set<string>>();
    for (const level of this.levels) {
      const plan = plans.get(level)!;
      const fresh = new Set<string>();
      mountedNow.set(level, fresh);
      for (const [child, parentKey] of plan.mounts) {
        const orderKey = this.orderKeyOf(level, child);
        level.parentOf.set(child, parentKey);
        level.orderOf.set(child, orderKey);
        level.order.insert(parentKey, orderKey, child);
        const el = level.shape.template(this.driver, child, this.ancestorKeys(level, parentKey));
        for (const attr of level.shape.attrs) {
          const v = this.resolveOne(attr.view, child);
          if (attr.presence) attr.apply(this.driver, el, v);
          else if (v !== undefined) attr.apply(this.driver, el, v);
        }
        level.nodes.set(child, el);
        this.driver.insertBefore(
          this.parentEl(level, parentKey),
          el,
          this.mountedSuccessor(level, parentKey, orderKey, child),
        );
        fresh.add(child);
      }
    }

    // Phase 3: field updates — a −/+ on an attribute view at a mounted,
    // not-freshly-mounted key is exactly one field application on the same
    // element (node identity preserved).
    for (const level of this.levels) {
      const fresh = mountedNow.get(level)!;
      for (const attr of level.shape.attrs) {
        const touched = touchedAttrs.get(attr.view);
        if (!touched) continue;
        for (const child of touched) {
          if (fresh.has(child)) continue;
          const el = level.nodes.get(child);
          if (el === undefined) continue;
          const v = this.resolveOne(attr.view, child);
          // A presence attribute is applied when its row *goes away* as well:
          // that retraction is exactly "turn the class off" (S-52). A value
          // attribute has nothing to apply without a value.
          if (attr.presence) attr.apply(this.driver, el, v);
          else if (v !== undefined) attr.apply(this.driver, el, v);
        }
      }
    }

    // Phase 4: moves and reparents, against final membership. A reparent
    // reuses the element — insertBefore of an attached node is a move, so
    // focus/selection/animation survive a Kanban drag across lists.
    for (const level of this.levels) {
      const plan = plans.get(level)!;
      const fresh = mountedNow.get(level)!;

      for (const [child, { from, to }] of plan.reparents) {
        const el = level.nodes.get(child);
        if (el === undefined) continue;
        const oldOrder = level.orderOf.get(child) ?? "";
        const newOrder = this.orderKeyOf(level, child);
        level.order.remove(from, oldOrder, child);
        level.order.insert(to, newOrder, child);
        level.parentOf.set(child, to);
        level.orderOf.set(child, newOrder);
        this.driver.insertBefore(
          this.parentEl(level, to),
          el,
          this.mountedSuccessor(level, to, newOrder, child),
        );
      }

      // Pure order-key moves: a −/+ on the order view for a child that is
      // mounted, wasn't just mounted, and isn't reparenting.
      const orderView = level.shape.orderView;
      if (!orderView) continue;
      const touched = touchedAttrs.get(orderView);
      if (!touched) continue;
      for (const child of touched) {
        if (fresh.has(child) || plan.reparents.has(child) || plan.removes.has(child)) continue;
        const el = level.nodes.get(child);
        if (el === undefined) continue;
        const parentKey = level.parentOf.get(child)!;
        const oldOrder = level.orderOf.get(child) ?? "";
        const newOrder = this.orderKeyOf(level, child);
        if (oldOrder === newOrder) continue;
        level.order.remove(parentKey, oldOrder, child);
        level.order.insert(parentKey, newOrder, child);
        level.orderOf.set(child, newOrder);
        this.driver.insertBefore(
          this.parentEl(level, parentKey),
          el,
          this.mountedSuccessor(level, parentKey, newOrder, child),
        );
      }
    }
  }

  // --- internals ----------------------------------------------------------

  private mirrorFor(view: string): Map<string, Map<string, number>> {
    let m = this.mirror.get(view);
    if (!m) {
      m = new Map();
      this.mirror.set(view, m);
    }
    return m;
  }

  /** The single present (weight > 0) value for a key in a functional view. */
  private resolveOne(view: string, key: string): string | undefined {
    const byValue = this.mirror.get(view)?.get(key);
    if (!byValue) return undefined;
    for (const [value, w] of byValue) {
      if (w > 0) return value;
    }
    return undefined;
  }

  private orderKeyOf(level: Level<El>, child: string): string {
    const view = level.shape.orderView;
    if (!view) return "";
    return this.resolveOne(view, child) ?? "";
  }

  private parentEl(level: Level<El>, parentKey: string): El {
    if (!level.parent) return this.container;
    const el = level.parent.nodes.get(parentKey);
    if (el === undefined) {
      throw new Error(
        `shape ${level.shape.name}: parent ${parentKey} of level ${level.parent.shape.name} is not mounted`,
      );
    }
    return el;
  }

  /** Whether any ancestor of (level, parentKey) is removed in this batch —
   *  the subtree-coalescing test. */
  private ancestorRemoved(
    level: Level<El>,
    parentKey: string,
    plans: Map<Level<El>, LevelPlan>,
  ): boolean {
    let lvl = level.parent;
    let key: string | undefined = parentKey;
    while (lvl && key !== undefined) {
      if (plans.get(lvl)!.removes.has(key)) return true;
      key = lvl.parentOf.get(key);
      lvl = lvl.parent;
    }
    return false;
  }

  /** The enclosing rows' keys for a child mounted under `parentKey`, nearest
   *  first — read from the (already updated) membership mirrors. */
  private ancestorKeys(level: Level<El>, parentKey: string): string[] {
    const keys: string[] = [];
    let lvl = level.parent;
    let key: string | undefined = parentKey;
    while (lvl && key !== undefined) {
      keys.push(key);
      key = lvl.parentOf.get(key);
      lvl = lvl.parent;
    }
    return keys;
  }

  /** The next *mounted* sibling after (orderKey, child) — the insertBefore
   *  reference. Skips index entries not yet in the DOM (later mounts of the
   *  same batch). */
  private mountedSuccessor(
    level: Level<El>,
    parentKey: string,
    orderKey: string,
    child: string,
  ): El | null {
    let probe: [string, string] = [orderKey, child];
    for (;;) {
      const next = level.order.successor(parentKey, probe[0], probe[1]);
      if (next === null) return null;
      const el = level.nodes.get(next);
      const nextOrder = level.orderOf.get(next) ?? "";
      if (el !== undefined) return el;
      probe = [nextOrder, next];
    }
  }
}

interface Level<El> {
  readonly shape: ShapeNode<El>;
  readonly parent: Level<El> | null;
  readonly depth: number;
  /** Integrated membership: child -> parent key. */
  parentOf: Map<string, string>;
  /** Integrated order keys: child -> fractional key ("" when unordered). */
  orderOf: Map<string, string>;
  /** Mounted elements. */
  nodes: Map<string, El>;
  order: OrderIndex;
}

interface LevelPlan {
  /** child -> parent to mount under. */
  mounts: Map<string, string>;
  removes: Set<string>;
  reparents: Map<string, { from: string; to: string }>;
}
