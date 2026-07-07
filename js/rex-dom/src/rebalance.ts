import { generateKeyBetween, generateNKeysBetween } from "fractional-indexing";

/**
 * Fractional order keys (nesting-draft §5). Key generation is the published
 * `fractional-indexing` library — deliberately not hand-rolled. The one
 * pathology is repeated insertion into the same gap (linear key growth), so
 * `rebalancePlan` detects oversized keys and re-spaces a parent's children.
 * The plan is applied by the *app* as ordinary base-table updates (the engine
 * has no ordering concept; order keys are data).
 */

/** A fresh key strictly between two neighbors (null = open end). */
export const keyBetween = generateKeyBetween;

/** Threshold above which a parent's keys get re-spaced. */
export const REBALANCE_LIMIT = 40;

/**
 * If any key exceeds the limit, a full re-spacing of the parent's children:
 * child -> new key, in the same order. Otherwise null. `ordered` is the
 * parent's children in current order, e.g. `OrderIndex.childrenOf(parent)`.
 */
export function rebalancePlan(
  ordered: readonly (readonly [orderKey: string, child: string])[],
  limit: number = REBALANCE_LIMIT,
): Map<string, string> | null {
  if (!ordered.some(([k]) => k.length > limit)) return null;
  const fresh = generateNKeysBetween(null, null, ordered.length);
  const plan = new Map<string, string>();
  ordered.forEach(([, child], i) => plan.set(child, fresh[i]!));
  return plan;
}
