import type { DomDriver } from "./types.js";

/** The real thing: a thin veneer over DOM calls. */
export class BrowserDriver implements DomDriver<HTMLElement> {
  createElement(tag: string): HTMLElement {
    return document.createElement(tag);
  }
  setText(el: HTMLElement, text: string): void {
    el.textContent = text;
  }
  setAttr(el: HTMLElement, name: string, value: string): void {
    el.setAttribute(name, value);
  }
  insertBefore(parent: HTMLElement, el: HTMLElement, ref: HTMLElement | null): void {
    parent.insertBefore(el, ref);
  }
  removeChild(parent: HTMLElement, el: HTMLElement): void {
    parent.removeChild(el);
  }
}

/**
 * A DOM-free element for tests: a real child list (so ordering assertions are
 * possible) plus text/attrs.
 */
export interface SpyEl {
  readonly tag: string;
  readonly id: number;
  text: string;
  attrs: Record<string, string>;
  children: SpyEl[];
  parent: SpyEl | null;
}

/**
 * Mutation-counting driver. The shaper's headline guarantees are counts:
 * a retitle is one setText on the same element, a reorder exactly one
 * insertBefore, a dead subtree exactly one removeChild.
 */
export class SpyDriver implements DomDriver<SpyEl> {
  counts = { createElement: 0, setText: 0, setAttr: 0, insertBefore: 0, removeChild: 0 };
  private nextId = 0;

  resetCounts(): void {
    this.counts = { createElement: 0, setText: 0, setAttr: 0, insertBefore: 0, removeChild: 0 };
  }

  createElement(tag: string): SpyEl {
    this.counts.createElement++;
    return { tag, id: this.nextId++, text: "", attrs: {}, children: [], parent: null };
  }
  setText(el: SpyEl, text: string): void {
    this.counts.setText++;
    el.text = text;
  }
  setAttr(el: SpyEl, name: string, value: string): void {
    this.counts.setAttr++;
    el.attrs[name] = value;
  }
  insertBefore(parent: SpyEl, el: SpyEl, ref: SpyEl | null): void {
    this.counts.insertBefore++;
    // Attached-node insertion is a move, exactly like the DOM.
    if (el.parent) {
      const sibs = el.parent.children;
      sibs.splice(sibs.indexOf(el), 1);
    }
    const at = ref ? parent.children.indexOf(ref) : parent.children.length;
    parent.children.splice(at < 0 ? parent.children.length : at, 0, el);
    el.parent = parent;
  }
  removeChild(parent: SpyEl, el: SpyEl): void {
    this.counts.removeChild++;
    const at = parent.children.indexOf(el);
    if (at >= 0) parent.children.splice(at, 1);
    el.parent = null;
  }
}
