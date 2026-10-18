import { useCallback, useEffect, type KeyboardEvent, type RefObject } from "react";

/**
 * Shared modal behaviour for the navigation drawer and the confirmation
 * dialog (docs/09 §11.6): focus moves into the panel, Tab/Shift+Tab wrap
 * inside it, Escape closes it, the rest of the page becomes inert and the
 * body stops scrolling, and focus returns to the invoking control.
 *
 * The panel must be rendered through a portal attached to `document.body`:
 * `inert` cannot be applied to the panel's ancestors, so the behaviour inerts
 * every other body child instead.
 */

const FOCUSABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

const NON_INERTABLE_TAGS = new Set(["SCRIPT", "STYLE", "LINK", "META", "TITLE", "BASE"]);

function focusableElements(container: HTMLElement | null): HTMLElement[] {
  if (!container) {
    return [];
  }
  return Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (element) => !element.hasAttribute("disabled") && element.tabIndex !== -1,
  );
}

/** Keeps Tab focus inside `container`; call from the panel's onKeyDown. */
export function trapTabKey(event: KeyboardEvent<HTMLElement>, container: HTMLElement | null): void {
  const focusable = focusableElements(container);
  if (focusable.length === 0) {
    event.preventDefault();
    return;
  }

  const first = focusable[0];
  const last = focusable[focusable.length - 1];
  if (event.shiftKey && (document.activeElement === first || document.activeElement === container)) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first.focus();
  }
}

/**
 * Reference-counted page lock. Two modals can be open at once (a guard dialog
 * opening over an open drawer/dialog); without the count the first one to
 * close would unlock the page under the other, or leave the other's portal
 * inert. The first lock inerts everything and takes the scroll lock; every
 * later lock only re-enables its own portal (the first lock may have inerted
 * it when both mounted together); only the last unlock restores the page.
 */
let lockDepth = 0;
let inertedElements: HTMLElement[] = [];

function portalOf(container: HTMLElement | null): HTMLElement | null {
  if (!container) {
    return null;
  }
  for (const child of Array.from(document.body.children)) {
    if (child instanceof HTMLElement && child.contains(container)) {
      return child;
    }
  }
  return null;
}

function lockPage(container: HTMLElement | null): () => void {
  if (lockDepth === 0) {
    const inerted: HTMLElement[] = [];
    for (const child of Array.from(document.body.children)) {
      if (!(child instanceof HTMLElement)) {
        continue;
      }
      // The portal root holding the modal panel must stay interactive.
      if (container && child.contains(container)) {
        continue;
      }
      if (NON_INERTABLE_TAGS.has(child.tagName)) {
        continue;
      }
      if (!child.hasAttribute("inert")) {
        child.setAttribute("inert", "");
        inerted.push(child);
      }
    }
    inertedElements = inerted;
    document.body.classList.add("nuwa-scroll-lock");
  } else {
    // A modal opened while the page was already locked: its portal may have
    // been inerted by the first lock (both mounted in one commit). Keep it
    // interactive; the unlock does not need to re-inert it because it is
    // unmounting or belongs to the still-open outer modal.
    const portal = portalOf(container);
    if (portal?.hasAttribute("inert")) {
      portal.removeAttribute("inert");
    }
  }
  lockDepth += 1;

  let released = false;
  return () => {
    if (released) {
      return;
    }
    released = true;
    lockDepth = Math.max(0, lockDepth - 1);
    if (lockDepth > 0) {
      return;
    }
    for (const element of inertedElements) {
      element.removeAttribute("inert");
    }
    inertedElements = [];
    document.body.classList.remove("nuwa-scroll-lock");
  };
}

export interface ModalBehaviorOptions {
  active: boolean;
  /** Focus trap boundary (the dialog/drawer panel). */
  containerRef: RefObject<HTMLElement | null>;
  /** Element focused on open; defaults to the first focusable element. */
  initialFocusRef?: RefObject<HTMLElement | null>;
  /** Element focused on close; defaults to the previously focused element. */
  returnFocusRef?: RefObject<HTMLElement | null>;
  onEscape?: () => void;
}

/**
 * Wires focus trapping, background inert/scroll lock and focus restoration.
 * Returns the panel keydown handler; attach it to the element that owns
 * `containerRef`.
 */
export function useModalBehavior({
  active,
  containerRef,
  initialFocusRef,
  returnFocusRef,
  onEscape,
}: ModalBehaviorOptions): (event: KeyboardEvent<HTMLElement>) => void {
  useEffect(() => {
    if (!active) {
      return;
    }
    const container = containerRef.current;
    const previouslyFocused =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const unlock = lockPage(container);

    const initial = initialFocusRef?.current ?? focusableElements(container)[0] ?? container;
    initial?.focus();

    return () => {
      // Unlock (remove inert) before restoring focus, otherwise the target
      // would still be inside an inert subtree.
      unlock();
      const target = returnFocusRef?.current ?? previouslyFocused;
      if (target && target.isConnected) {
        target.focus();
      }
    };
  }, [active, containerRef, initialFocusRef, returnFocusRef]);

  return useCallback(
    (event: KeyboardEvent<HTMLElement>) => {
      if (event.key === "Escape" && onEscape) {
        event.stopPropagation();
        onEscape();
        return;
      }
      if (event.key === "Tab") {
        trapTabKey(event, containerRef.current);
      }
    },
    [containerRef, onEscape],
  );
}
