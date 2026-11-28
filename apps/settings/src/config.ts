import { invoke } from "@tauri-apps/api/core";

export type Language = "auto" | "es" | "en" | "fr" | "zh" | "hi" | "ar" | "bn" | "pt";
export type IslandStyle = "notch" | "bubble";
export type GenieSpeed = "normal" | "fast" | "faster";
export type MascotCharacter = "crab" | "cat" | "fox" | "capybara" | "mochi" | "girl";
export type DockPosition = "left" | "bottom" | "right";
export type DockDensity = "compact" | "normal" | "spacious";
export type DockAutoHideSpeed = "smooth" | "normal" | "fast";
/** View of the existing two settings; saved atomically as a mutually exclusive pair. */
export type DockHidingMode = "visible" | "automatic" | "intelligent";
/** `auto` follows Windows' light or dark mode. */
export type Appearance = "auto" | "light" | "dark" | "frosted" | "clear" | "liquidGlass";

export type SearchMonitor = "mouse" | "active" | "primary";
export type SearchEngine = "google" | "bing" | "duckduckgo" | "brave" | "ecosia" | "startpage" | "custom";
export interface SearchKeyword {
  word: string;
  url: string;
}

/** Mirrors `muelle_config::SearchConfig`. */
export interface SearchConfig {
  enabled: boolean;
  hotkey: string;
  monitor: SearchMonitor;
  keepQuery: boolean;
  closeOnBlur: boolean;
  width: number;
  position: number;
  maxRows: number;
  compact: boolean;
  headers: boolean;
  showApps: boolean;
  showSystemSettings: boolean;
  showFolders: boolean;
  showFiles: boolean;
  showContents: boolean;
  showDefinitions: boolean;
  showCalculator: boolean;
  showAddresses: boolean;
  showWebSearch: boolean;
  contentResults: number;
  learn: boolean;
  standardFolders: Record<"desktop" | "documents" | "downloads" | "pictures" | "music" | "videos", boolean>;
  folders: string[];
  excludedFolders: string[];
  hiddenFiles: boolean;
  refreshMinutes: number;
  engine: SearchEngine;
  customSearch: string;
  keywordsEnabled: boolean;
  keywords: SearchKeyword[];
}

/** The ranges `muelle_config` keeps these settings within. */
export const SEARCH_LIMITS = {
  width: { min: 520, max: 960 },
  position: { min: 0, max: 40 },
  maxRows: { min: 4, max: 12 },
  contentResults: { min: 1, max: 6 },
} as const;

/** Mirrors `muelle_config::WeekStart`. */
export type WeekStart = "auto" | "monday" | "sunday" | "saturday";
/** Mirrors `muelle_config::MenuBarIcon`. */
export type MenuBarIcon = "ring" | "windows" | "custom";
/** Why a picture was refused: `muelle_shell::menu_icon::Problem` codes. */
export type IconProblem = "format" | "size" | "square" | "dimensions" | "save";

/** The control center's tiles, in the order it lays them out. */
export const CONTROL_TILES = [
  "network", "media", "bluetooth", "nearbySharing", "focus", "taskView", "cast", "display",
  "sound", "darkMode", "calculator", "timer", "screenshot",
] as const;
/** Mirrors `muelle_config::ControlTiles`. */
export type ControlTiles = Record<(typeof CONTROL_TILES)[number], boolean>;

/** Mirrors `muelle_config::Config`. */
export interface Config {
  version: number;
  pinned: string[];
  recent: string[];
  autoHide: boolean;
  smartHide: boolean;
  iconSize: number;
  magnification: boolean;
  magnifiedSize: number;
  dockPosition: DockPosition;
  dockDensity: DockDensity;
  dockAutoHideSpeed: DockAutoHideSpeed;
  showStart: boolean;
  showTrash: boolean;
  showDownloads: boolean;
  pinExplorer: boolean;
  genie: boolean;
  genieSpeed: GenieSpeed;
  showMinimized: boolean;
  showOtherWorkspaces: boolean;
  jumpLists: boolean;
  menuBar: boolean;
  menuBarAutoHide: boolean;
  menuBarSmartHide?: boolean;
  aiUsage: boolean;
  menuBarMusic: boolean;
  menuBarIslandStyle: IslandStyle;
  /** A bell at the right end of the menu bar that opens the notification center. */
  menuBarNotifications: boolean;
  /** What the clock opens (an app, shortcut or URI); null for the notification center. */
  menuBarClockTarget: string | null;
  accessoryBattery: boolean;
  menuBarSystemMonitor?: boolean;
  showIndicators: boolean;
  showRecents: boolean;
  bounceOnOpen: boolean;
  mascot: boolean;
  mascotCharacter?: MascotCharacter;
  mascotBreakReminder?: boolean;
  checkUpdates: boolean;
  autoUpdate: boolean;
  language: Language;
  appearance: Appearance;
  /** The menu bar's own theme; null (or missing) gives it the dock's. */
  menuBarAppearance: Appearance | null;
  /** The folder shown at the right of the dock, whose contents open as a menu. */
  shortcutsFolder: string | null;
  search: SearchConfig;
  menuBarCircle?: boolean;
  menuBarCircleMenu?: boolean;
  menuBarIcon?: MenuBarIcon;
  menuBarIconFile?: string | null;
  menuBarIconAnimation?: boolean;
  menuBarDynamicArrow?: boolean;
  controlCenter?: boolean;
  controlCenterTiles?: ControlTiles;
  networkMenu?: boolean;
  trayMenu?: boolean;
  calendar?: boolean;
  calendarWeekStart?: WeekStart;
}

export const ICON_SIZE = { min: 32, max: 96 } as const;
export const MAX_MAGNIFIED_SIZE = 160;

export const loadConfig = () => invoke<Config>("get_config");
export const defaultConfig = () => invoke<Config>("default_config");
export const saveConfig = (patch: Partial<Config>, previewMascot = false) => invoke<Config>("set_config", { patch, previewMascot });
export const getAutostart = () => invoke<boolean>("get_autostart");
/** Current Windows values for display only; never merge these into Config. */
export interface WindowsPreferences {
  animations: boolean;
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
/** An app or shortcut for the clock to open, or null if the user cancels. */
export const pickClockTarget = (current: string | null, apps: string, all: string) =>
  invoke<string | null>("pick_clock_target", { current, apps, all });
/** The kept copy of the picture chosen; null if the user cancels. */
export const pickMenuIcon = (current: string | null, images: string) =>
  invoke<string | null>("pick_menu_icon", { current, images });
export const menuIconPreview = (path: string) => invoke<string | null>("menu_icon_preview", { path });
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
let wallpaperSelectionRequest: Promise<number | null> | undefined;
/** Share overlapping reads from StrictMode or quick section navigation. */
export const loadWallpaperSelection = () => {
  if (wallpaperSelectionRequest) return wallpaperSelectionRequest;
  const request = invoke<number | null>("wallpaper_selection").finally(() => {
    if (wallpaperSelectionRequest === request) wallpaperSelectionRequest = undefined;
  });
  wallpaperSelectionRequest = request;
  return request;
};
export const applyWallpaper = async (id: number, preview: string) => {
  await invoke<void>("apply_wallpaper", { id });
  // Original images stay in native storage; Settings uses only the small preview.
  wallpaperRequest = Promise.resolve(preview);
  wallpaperSelectionRequest = undefined;
};
export const installUpdate = () => invoke<void>("install_update");

export interface SearchStatus {
  running: boolean;
  /** The shortcut in effect; null if none could be claimed. */
  hotkey: string | null;
  /** Whether definitions have a dictionary to come from. */
  dictionary: boolean;
}
export const loadSearchStatus = () => invoke<SearchStatus>("search_status");
/** Releases the search shortcut while a new one is typed, then claims it again. */
export const suspendSearchHotkey = (suspend: boolean) => invoke<void>("suspend_search_hotkey", { suspend });
export const rebuildSearchIndex = () => invoke<void>("rebuild_search_index");
export const clearSearchHistory = () => invoke<void>("clear_search_history");
/** The section Settings was started to show, if any. */
export const initialSection = () => invoke<string | null>("initial_section");
