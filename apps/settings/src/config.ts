import { invoke } from "@tauri-apps/api/core";

export type Language = "auto" | "es" | "en" | "fr";
export type NotificationPosition = "top-right" | "bottom-right";
export type IslandStyle = "notch" | "bubble";
export type GenieSpeed = "normal" | "fast" | "faster";
export type DockPosition = "left" | "bottom" | "right";

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
  menuBarIsland: boolean;
  aiUsage: boolean;
  menuBarMusic: boolean;
  menuBarIslandStyle: IslandStyle;
  accessoryBattery: boolean;
  menuBarSearch: boolean | null;
  menuBarTaskView: boolean | null;
  menuBarWidgets: boolean | null;
  menuBarBluetooth: boolean;
  menuBarBatteryPercent: boolean | null;
  controlCenter: boolean;
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
/** Windows' animation setting; the genie only plays while it is on. */
export const windowsAnimations = () => invoke<boolean>("windows_animations");
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
