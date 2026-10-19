export const desktopPages = ["status", "notifications", "rate", "proxies", "products", "settings"] as const;
export type DesktopPage = typeof desktopPages[number];
export type DesktopAction = DesktopPage | "find-products" | "check-updates";

export function isMacDesktop() {
  return /Mac/.test(navigator.platform);
}

export function desktopShortcut(event: KeyboardEvent, mac = isMacDesktop()): DesktopAction | null {
  if (event.defaultPrevented || event.isComposing || event.repeat || event.altKey || event.shiftKey) return null;
  if (mac ? !event.metaKey || event.ctrlKey : !event.ctrlKey || event.metaKey) return null;
  if (event.key === ",") return "settings";
  if (event.key.toLowerCase() === "f") return "find-products";
  const index = Number(event.key) - 1;
  return /^[1-6]$/.test(event.key) ? desktopPages[index] : null;
}

export function nextSidebarIndex(key: string, current: number, count: number): number | null {
  if (key === "Home") return 0;
  if (key === "End") return count - 1;
  if (key === "ArrowUp") return Math.max(0, current - 1);
  if (key === "ArrowDown") return Math.min(count - 1, current + 1);
  return null;
}

export function allowsContextMenu(target: EventTarget | null) {
  return target instanceof Element && !!target.closest(
    'input, textarea, [contenteditable="true"], [data-selectable], pre, code, .message-table, .drawer-records',
  );
}
