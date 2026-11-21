import { invoke } from "@tauri-apps/api/core";

export type Language = "auto" | "es" | "en" | "fr";
export type NotificationPosition = "top-right" | "bottom-right";
export type IslandStyle = "notch" | "bubble";
export type GenieSpeed = "normal" | "fast" | "faster";
export type DockPosition = "left" | "bottom" | "right";
export type MenuBarItem =
  | "control-center" | "focus" | "wifi" | "bluetooth" | "sound" | "battery"
  | "input-language" | "hidden-icons" | "search" | "task-view" | "widgets" | "privacy";

/** Left to right, as macOS lays them out. Mirrors `MenuBarItem::DEFAULT_ORDER`. */
export const MENU_BAR_DEFAULT_ORDER: readonly MenuBarItem[] = [
  "widgets", "task-view", "search", "hidden-icons", "input-language",
  "bluetooth", "battery", "sound", "wifi", "focus", "control-center", "privacy",
];

/**
 * Mirrors `effective_menu_bar_order`: the saved order without duplicates or
 * unknown items, each missing item placed after the nearest preceding default
 * item that is present (or first).
 */
export function effectiveMenuBarOrder(saved: readonly MenuBarItem[] | undefined): MenuBarItem[] {
  const order: MenuBarItem[] = [];
  for (const item of saved ?? []) {
    if (MENU_BAR_DEFAULT_ORDER.includes(item) && !order.includes(item)) order.push(item);
  }
  MENU_BAR_DEFAULT_ORDER.forEach((item, index) => {
    if (order.includes(item)) return;
    let at = 0;
    for (let previous = index - 1; previous >= 0; previous--) {
      const found = order.indexOf(MENU_BAR_DEFAULT_ORDER[previous]);
      if (found >= 0) { at = found + 1; break; }
    }
    order.splice(at, 0, item);
  });
  return order;
}

/** Mirrors `muelle_config::Config`. */
export interface Config {
  version: number;
  pinned: string[];
  recent: string[];
  autoHide: boolean;
  iconSize: number;
  magnification: boolean;
  magnifiedSize: number;
  dockPosition: DockPosition;
  genie: boolean;
  genieSpeed: GenieSpeed;
  showMinimized: boolean;
  showPreviewCloseButton: boolean;
  menuBar: boolean;
  menuBarAutoHide: boolean;
  menuBarGlass: boolean;
  aiUsage: boolean;
  menuBarMusic: boolean;
  menuBarIslandStyle: IslandStyle;
  accessoryBattery: boolean;
  menuBarSearch: boolean | null;
  menuBarTaskView: boolean | null;
  menuBarWidgets: boolean | null;
  menuBarBluetooth: boolean;
  menuBarBatteryPercent: boolean | null;
  menuBarTrayPopup: boolean;
  /** Left to right; empty means the default order. May omit items. */
  menuBarOrder: MenuBarItem[];
  /**
   * Items turned off. Bluetooth, Search, Task View and Widgets keep their own
   * fields as the source of truth and never appear here.
   */
  menuBarHidden: MenuBarItem[];
  clockShowDate: boolean;
  clockShowWeekday: boolean;
  clockSeconds: boolean;
  /** `null` follows Windows. */
  clock24h: boolean | null;
  /** 0 (clear) to 100 (frosted). */
  menuBarGlassStrength: number;
  showIndicators: boolean;
  showRecents: boolean;
  bounceOnOpen: boolean;
  notifications: boolean;
  notificationPosition: NotificationPosition;
  notificationDuration: number;
  notificationBadges: boolean;
  notificationBounce: boolean;
  notificationDnd: boolean;
  mutedApps: string[];
  mascot: boolean;
  checkUpdates: boolean;
  autoUpdate: boolean;
  language: Language;
  /** The folder shown at the right of the dock, whose contents open as a menu. */
  shortcutsFolder: string | null;
}

export const ICON_SIZE = { min: 32, max: 96 } as const;
export const MAX_MAGNIFIED_SIZE = 160;

export const loadConfig = () => invoke<Config>("get_config");
export const defaultConfig = () => invoke<Config>("default_config");
export const saveConfig = (patch: Partial<Config>) => invoke<Config>("set_config", { patch });
export const getAutostart = () => invoke<boolean>("get_autostart");
/** Current Windows values for display only; never merge these into Config. */
export interface WindowsPreferences {
  animations: boolean;
  menuBarSearch: boolean;
  menuBarTaskView: boolean;
  menuBarWidgets: boolean;
  menuBarBatteryPercent: boolean;
}

let windowsPreferencesRequest: Promise<WindowsPreferences> | undefined;
/** Coalesce overlapping focus/StrictMode reads, but reread after returning from Windows Settings. */
export const loadWindowsPreferences = () =>
  windowsPreferencesRequest ??= invoke<WindowsPreferences>("windows_preferences")
    .finally(() => { windowsPreferencesRequest = undefined; });
export const openAnimationSettings = () => invoke<void>("open_animation_settings");
export const setAutostart = (enabled: boolean) => invoke<void>("set_autostart", { enabled });
/** The folder the user picks, or null if they cancel. */
export const pickFolder = (current: string | null) =>
  invoke<string | null>("pick_folder", { current });
export interface UpdateInfo {
  version: string;
  /** Release page with the notes, if the manifest has one. */
  notes: string | null;
}

/** Resolves to the newer version available, or null when up to date. */
export const checkUpdate = () => invoke<UpdateInfo | null>("check_update");
export const openReleaseNotes = (url: string) => invoke<void>("open_release_notes", { url });
/** Fixed project URL, opened only when the user asks. */
export const openRepository = () => invoke<void>("open_repository");
let wallpaperRequest: Promise<string | null> | undefined;
/** One bounded native read per Settings process, including React StrictMode. */
export const loadWallpaperPreview = () =>
  wallpaperRequest ??= invoke<string | null>("wallpaper_preview").catch(() => null);
export const installUpdate = () => invoke<void>("install_update");
