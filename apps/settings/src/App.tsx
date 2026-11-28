import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  type Appearance,
  type Config,
  type DockPosition,
  type DockDensity,
  type DockAutoHideSpeed,
  type DockHidingMode,
  type GenieSpeed,
  type MascotCharacter,
  type IslandStyle,
  type Language,
  type UpdateInfo,
  type WindowsPreferences,
  ICON_SIZE,
  MAX_MAGNIFIED_SIZE,
  CONTROL_TILES,
  type ControlTiles,
  type IconProblem,
  type MenuBarIcon,
  type WeekStart,
  menuIconPreview,
  pickMenuIcon,
  checkUpdate,
  defaultConfig,
  getAutostart,
  initialSection,
  openAnimationSettings,
  loadWindowsPreferences,
  installUpdate,
  loadConfig,
  loadWallpaperPreview,
  openReleaseNotes,
  openRepository,
  pickClockTarget,
  pickFolder,
  saveConfig,
  setAutostart,
} from "./config";
import { Card, Row, Segmented, Select, Slider, SubRow, Toggle, VisualChoices } from "./controls";
import { DockDensityPreview, DockPreview, IslandPreview, MascotPreview, MenuIconPreview, SettingIcon, SpeedPreview, ThemePreview } from "./visuals";
import { type SectionId, SectionIcon } from "./icons";
import { SearchSection } from "./SearchSection";
import { UpdateBanner } from "./UpdateBanner";
import { WallpaperGallery } from "./WallpaperGallery";
import { WeatherSettings } from "./WeatherSettings";
import { type Texts, LANGUAGE_OPTIONS, resolveLanguage, textsFor } from "./i18n";

type UpdateState =
  | { kind: "off" }
  | { kind: "checking" }
  | { kind: "current" }
  | { kind: "available"; info: UpdateInfo }
  | { kind: "installing"; info: UpdateInfo; progress: number | null }
  | { kind: "installFailed"; info: UpdateInfo }
  | { kind: "error" };

const toUpdateState = (found: UpdateInfo | null): UpdateState =>
  found ? { kind: "available", info: found } : { kind: "current" };

/** An update is waiting for the user: the banner and the badge show. */
const pendingUpdate = (state: UpdateState): UpdateInfo | null =>
  state.kind === "available" || state.kind === "installing" || state.kind === "installFailed"
    ? state.info
    : null;

interface ProgressEvent {
  downloaded: number;
  total: number | null;
}

/** Dragging a slider saves at most this often; the dock redraws on each save. */
const SAVE_DELAY_MS = 60;
/** How long the reset button stays armed after the first click. */
const RESET_CONFIRM_MS = 4000;

export function App() {
  const [config, setConfig] = useState<Config | null>(null);
  const [autostart, setAutostartState] = useState(false);
  const [windowsPreferences, setWindowsPreferences] = useState<WindowsPreferences | null>(null);
  // Keep the animation notice from flashing while the initial system read is pending.
  const animations = windowsPreferences?.animations ?? true;
  const [section, setSection] = useState<SectionId>("dock");
  const [failed, setFailed] = useState(false);
  const [version, setVersion] = useState("");
  const [updateState, setUpdateState] = useState<UpdateState>({ kind: "checking" });
  const [confirmingReset, setConfirmingReset] = useState(false);
  const [projectError, setProjectError] = useState(false);
  const [wallpaper, setWallpaper] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    loadWallpaperPreview().then((image) => { if (alive) setWallpaper((current) => current ?? image); });
    return () => { alive = false; };
  }, []);
  /** "Later" hides the banner until Settings is opened again. */
  const [bannerDismissed, setBannerDismissed] = useState(false);
  const pending = useRef<Partial<Config> | null>(null);
  const pendingMascotPreview = useRef(false);
  const writes = useRef(Promise.resolve(true));
  const timer = useRef<number | undefined>(undefined);
  const resetTimer = useRef<number | undefined>(undefined);

  const flushSettings = useCallback(() => {
    window.clearTimeout(timer.current);
    if (pending.current) {
      writes.current = writes.current.then(async () => {
        // Take the patch when its turn starts. A failed earlier write can then
        // rejoin the queue without overwriting a newer choice for the same key.
        const latest = pending.current;
        if (!latest) return true;
        pending.current = null;
        const previewMascot = pendingMascotPreview.current;
        pendingMascotPreview.current = false;
        try {
          await saveConfig(latest, previewMascot);
          setFailed(false);
          return true;
        } catch {
          const newerMascotChoice = pending.current && "mascotCharacter" in pending.current;
          pending.current = Object.assign({}, latest, pending.current);
          if (!newerMascotChoice) pendingMascotPreview.current ||= previewMascot;
          setFailed(true);
          return false;
        }
      });
    }
    return writes.current;
  }, []);

  useEffect(() => {
    let alive = true;
    let focusRead = 0;
    const readWindowsPreferences = () => {
      loadWindowsPreferences()
        .then((values) => { if (alive) setWindowsPreferences(values); })
        .catch(() => { });
    };
    const refreshOnFocus = async () => {
      const read = ++focusRead;
      readWindowsPreferences();
      const queued = flushSettings();
      if (!await queued || !alive || pending.current || writes.current !== queued) return;
      const loaded = await loadConfig();
      // A dock menu may have hidden an item. Never replace a newer local edit
      // or a newer focus read with an older snapshot from the native bridge.
      if (alive && read === focusRead && !pending.current && writes.current === queued) {
        setConfig(loaded);
      }
    };
    const onFocus = () => { void refreshOnFocus().catch(() => { }); };
    readWindowsPreferences();
    window.addEventListener("focus", onFocus);
    Promise.all([loadConfig(), getAutostart()])
      .then(([loaded, starts]) => {
        if (!alive) return;
        setConfig(loaded);
        setAutostartState(starts);
        // The only network access in Muelle, and the user can turn it off.
        if (!loaded.checkUpdates) {
          setUpdateState({ kind: "off" });
          return;
        }
        checkUpdate()
          .then((found) => alive && setUpdateState(toUpdateState(found)))
          .catch(() => alive && setUpdateState({ kind: "error" }));
      })
      .catch(() => alive && setFailed(true));
    // The search window's "settings" result opens this app at its page.
    initialSection()
      .then((wanted) => { if (alive && wanted === "search") setSection("search"); })
      .catch(() => { });
    const shown = listen<string>("show-section", ({ payload }) => {
      if (alive && payload === "search") setSection("search");
    });
    getVersion()
      .then((current) => alive && setVersion(current))
      .catch(() => { });
    const progress = listen<ProgressEvent>("update-progress", ({ payload }) => {
      const share = payload.total ? Math.min(payload.downloaded / payload.total, 1) : null;
      setUpdateState((state) => (state.kind === "installing" ? { ...state, progress: share } : state));
    });
    let closing = false;
    const close = getCurrentWindow().onCloseRequested(async (event) => {
      if (!alive || closing) {
        event.preventDefault();
        return;
      }
      closing = true;
      // A change can be buffered or queued while an earlier write finishes.
      let drained: Promise<boolean>;
      do {
        drained = flushSettings();
        if (!await drained) {
          event.preventDefault();
          closing = false;
          return;
        }
      } while (pending.current || writes.current !== drained);
    });
    return () => {
      alive = false;
      window.removeEventListener("focus", onFocus);
      window.clearTimeout(timer.current);
      window.clearTimeout(resetTimer.current);
      progress.then((unlisten) => unlisten()).catch(() => { });
      shown.then((unlisten) => unlisten()).catch(() => { });
      close.then((unlisten) => unlisten()).catch(() => { });
    };
  }, [flushSettings]);

  // Shows the change at once and saves the latest value shortly after, so a
  // slider drag does not write the file on every pixel.
  const update = useCallback((patch: Partial<Config>, previewMascot = false) => {
    setConfig((current) => {
      if (!current) return current;
      const next = { ...current, ...patch };
      if (next.magnifiedSize < next.iconSize) next.magnifiedSize = next.iconSize;
      return next;
    });
    pending.current = { ...pending.current, ...patch };
    if ("mascotCharacter" in patch) pendingMascotPreview.current = previewMascot;
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => void flushSettings(), SAVE_DELAY_MS);
  }, [flushSettings]);

  const changeAutostart = useCallback((enabled: boolean) => {
    setAutostartState(enabled);
    setAutostart(enabled).catch(() => {
      setAutostartState(!enabled);
      setFailed(true);
    });
  }, []);

  const recheck = useCallback(() => {
    setUpdateState({ kind: "checking" });
    checkUpdate()
      .then((found) => setUpdateState(toUpdateState(found)))
      .catch(() => setUpdateState({ kind: "error" }));
  }, []);

  const install = useCallback((info: UpdateInfo) => {
    setUpdateState({ kind: "installing", info, progress: null });
    // On success the installer replaces this app and this process exits.
    installUpdate().catch(() => setUpdateState({ kind: "installFailed", info }));
  }, []);

  const showNotes = useCallback((info: UpdateInfo) => {
    if (info.notes) openReleaseNotes(info.notes).catch(() => { });
  }, []);

  const showProject = useCallback(() => {
    openRepository().then(() => setProjectError(false)).catch(() => setProjectError(true));
  }, []);

  // Two clicks: the first arms the button, the second resets. Pinned and
  // recent apps stay; they belong to the user, not to the settings.
  const reset = useCallback(() => {
    if (!confirmingReset) {
      setConfirmingReset(true);
      resetTimer.current = window.setTimeout(() => setConfirmingReset(false), RESET_CONFIRM_MS);
      return;
    }
    window.clearTimeout(resetTimer.current);
    setConfirmingReset(false);
    defaultConfig()
      .then(({ pinned: _pinned, recent: _recent, shortcutsFolder: _folder, ...defaults }) =>
        // The folders and keywords the user added are theirs, too.
        update(config?.search ? {
          ...defaults,
          search: {
            ...defaults.search,
            folders: config.search.folders,
            excludedFolders: config.search.excludedFolders,
            keywords: config.search.keywords,
          },
        } : defaults),
      )
      .catch(() => setFailed(true));
  }, [config, confirmingReset, update]);

  const [iconPreview, setIconPreview] = useState<string | null>(null);
  const [iconProblem, setIconProblem] = useState<IconProblem | null>(null);
  const iconFile = config?.menuBarIconFile;
  useEffect(() => {
    let alive = true;
    setIconPreview(null);
    if (iconFile) {
      menuIconPreview(iconFile)
        .then((picture) => { if (alive) setIconPreview(picture); })
        .catch(() => { });
    }
    return () => { alive = false; };
  }, [iconFile]);

  const appearance = config?.appearance ?? "auto";
  useEffect(() => {
    getCurrentWindow().setTheme(windowTheme(appearance)).catch(() => { });
  }, [appearance]);

  if (!config) return <div className="window" />;
  const language = resolveLanguage(config.language, config.language === "auto" ? navigator.language : "");
  const t = textsFor(language);
  const durationFormat = new Intl.NumberFormat(config.language === "auto" ? navigator.language : config.language);
  const chooseFolder = () => {
    pickFolder(config.shortcutsFolder)
      .then((folder) => folder && update({ shortcutsFolder: folder }))
      .catch(() => setFailed(true));
  };
  const chooseClockTarget = () => {
    pickClockTarget(config.menuBarClockTarget ?? null, t.clockTargetApps, t.clockTargetAllFiles)
      .then((target) => target && update({ menuBarClockTarget: target }))
      .catch(() => setFailed(true));
  };
  const chooseMenuIcon = () => {
    pickMenuIcon(config.menuBarIconFile ?? null, t.menuBarIconImages)
      .then((file) => {
        setIconProblem(null);
        if (file) update({ menuBarIconFile: file });
      })
      .catch((code) => setIconProblem(ICON_PROBLEMS.find((problem) => problem === code) ?? "save"));
  };
  const menuIcon = config.menuBarIcon ?? "ring";
  const iconShown = config.menuBarCircle ?? true;
  const ownMenuBarTheme = config.menuBarAppearance != null;
  const themeChoices = THEMES.map((theme) => ({
    value: theme,
    label: themeName(t, theme),
    detail: theme === "auto" || theme === "frosted" || theme === "liquidGlass" ? t.themeAutoDetail : undefined,
    preview: <ThemePreview appearance={theme} wallpaper={wallpaper} />,
  }));
  const appWindow = getCurrentWindow();
  const waiting = pendingUpdate(updateState);
  // An update the user asked for keeps its banner even after "Later".
  const showBanner =
    waiting !== null && (!bannerDismissed || updateState.kind !== "available");

  return (
    <div className="window" lang={language} dir={language === "ar" ? "rtl" : "ltr"}>
      <aside className="sidebar" data-tauri-drag-region>
        <div className="traffic">
          <button
            type="button"
            className="light light-close"
            aria-label={t.close}
            onClick={() => appWindow.close()}
          >
            <svg viewBox="0 0 12 12" className="light-icon" aria-hidden="true">
              <path
                d="M 3.5 3.5 L 8.5 8.5 M 8.5 3.5 L 3.5 8.5"
                stroke="#4c0002"
                strokeWidth="1.2"
                strokeLinecap="round"
              />
            </svg>
          </button>
          <button
            type="button"
            className="light light-minimize"
            aria-label={t.minimize}
            onClick={() => appWindow.minimize()}
          >
            <svg viewBox="0 0 12 12" className="light-icon" aria-hidden="true">
              <path
                d="M 2.5 6 L 9.5 6"
                stroke="#995700"
                strokeWidth="1.3"
                strokeLinecap="round"
              />
            </svg>
          </button>
          <button
            type="button"
            className="light light-zoom"
            aria-label={t.zoom}
            onClick={() => appWindow.toggleMaximize()}
          >
            <svg viewBox="0 0 12 12" className="light-icon" aria-hidden="true">
              <path
                d="M 9.5 2.5 L 9.5 6.5 L 5.5 2.5 Z M 2.5 9.5 L 2.5 5.5 L 6.5 9.5 Z"
                fill="#006500"
              />
            </svg>
          </button>
        </div>
        <nav>
          {SECTIONS.map((id) => (
            <NavItem
              key={id}
              section={id}
              active={section === id}
              onSelect={() => setSection(id)}
              badge={id === "system" && waiting ? t.updateBadge : undefined}
            >
              {sectionTitle(t, id)}
            </NavItem>
          ))}
        </nav>
        <footer className="sidebar-footer">
          <button type="button" className="project-link" onClick={showProject}>
            <SettingIcon name="star" size={20} />
            <span><strong>{t.githubTitle}</strong><span>{t.githubStar}</span></span>
            <SettingIcon name="external" size={12} />
          </button>
          {projectError ? <p className="project-error" role="alert">{t.githubError}</p> : null}
          <div className="sidebar-version"><span>Muelle</span><span>{version ? `v${version}` : ""}</span></div>
        </footer>
      </aside>

      <main className="content">
        <header className="content-title" data-tauri-drag-region>
          {sectionTitle(t, section)}
        </header>
        {showBanner && waiting ? (
          <UpdateBanner
            t={t}
            info={waiting}
            progress={updateState.kind === "installing" ? updateState.progress : null}
            installing={updateState.kind === "installing"}
            failed={updateState.kind === "installFailed"}
            onInstall={() => install(waiting)}
            onLater={() => setBannerDismissed(true)}
            onNotes={() => showNotes(waiting)}
          />
        ) : null}
        <div className="content-body" key={section}>
          {section === "appearance" ? (
            <>
              <Card>
                <VisualChoices<Appearance>
                  label={ownMenuBarTheme ? t.themeDock : t.theme}
                  columns={3}
                  value={config.appearance ?? "auto"}
                  options={themeChoices}
                  onChange={(appearance) => update({ appearance })}
                />
                <p className="card-note row-hint">{ownMenuBarTheme ? t.themeDockHint : t.themeHint}</p>
                <Row icon={<SettingIcon name="menu" />} label={t.menuBarThemeSame} hint={t.menuBarThemeSameHint}>
                  <Toggle
                    label={t.menuBarThemeSame}
                    checked={!ownMenuBarTheme}
                    // Turning it off starts from the dock's theme, so nothing
                    // changes on screen until another theme is chosen.
                    onChange={(same) => update({ menuBarAppearance: same ? null : config.appearance ?? "auto" })}
                  />
                </Row>
                <SubRow label={t.menuBarTheme} stacked open={ownMenuBarTheme}>
                  <VisualChoices<Appearance>
                    label={t.menuBarTheme}
                    hideLabel
                    columns={3}
                    value={config.menuBarAppearance ?? config.appearance ?? "auto"}
                    options={themeChoices}
                    onChange={(menuBarAppearance) => update({ menuBarAppearance })}
                  />
                </SubRow>
              </Card>
              <WallpaperGallery texts={t} onApplied={setWallpaper} />
            </>
          ) : section === "dock" ? (
            <>
              <Card title={t.groupAppearance}>
                <VisualChoices<DockPosition>
                  label={t.dockPosition}
                  value={config.dockPosition}
                  options={[
                    { value: "left", label: t.dockPositionLeft, preview: <DockPreview position="left" iconSize={config.iconSize} wallpaper={wallpaper} /> },
                    { value: "bottom", label: t.dockPositionBottom, preview: <DockPreview position="bottom" iconSize={config.iconSize} wallpaper={wallpaper} /> },
                    { value: "right", label: t.dockPositionRight, preview: <DockPreview position="right" iconSize={config.iconSize} wallpaper={wallpaper} /> },
                  ]}
                  onChange={(dockPosition) => update({ dockPosition })}
                />
                <Row label={t.size}>
                  <Slider
                    label={t.size}
                    value={config.iconSize}
                    min={ICON_SIZE.min}
                    max={ICON_SIZE.max}
                    onChange={(iconSize) => update({ iconSize })}
                  />
                </Row>
                <VisualChoices<DockDensity>
                  label={t.dockDensity}
                  value={config.dockDensity ?? "normal"}
                  options={[
                    { value: "compact", label: t.dockDensityCompact, preview: <DockDensityPreview density="compact" /> },
                    { value: "normal", label: t.dockDensityNormal, preview: <DockDensityPreview density="normal" /> },
                    { value: "spacious", label: t.dockDensitySpacious, preview: <DockDensityPreview density="spacious" /> },
                  ]}
                  onChange={(dockDensity) => update({ dockDensity })}
                />
                <Row icon={<SettingIcon name="magnification" />} label={t.magnification} hint={t.magnificationHint}>
                  <Toggle
                    label={t.magnification}
                    checked={config.magnification}
                    onChange={(magnification) => update({ magnification })}
                  />
                </Row>
                <SubRow label={t.magnifiedSize} open={config.magnification}>
                  <Slider
                    label={t.magnifiedSize}
                    value={config.magnifiedSize}
                    min={config.iconSize}
                    max={MAX_MAGNIFIED_SIZE}
                    onChange={(magnifiedSize) => update({ magnifiedSize })}
                  />
                </SubRow>
              </Card>
              <Card title={t.groupBehavior}>
                <Row label={t.dockHidingMode} hint={config.autoHide ? t.dockHidingAutomaticHint : config.smartHide ? t.dockHidingIntelligentHint : t.dockHidingVisibleHint}>
                  <Select<DockHidingMode>
                    label={t.dockHidingMode}
                    value={config.autoHide ? "automatic" : config.smartHide ? "intelligent" : "visible"}
                    options={[
                      { value: "visible", label: t.dockHidingVisible },
                      { value: "automatic", label: t.dockHidingAutomatic },
                      { value: "intelligent", label: t.dockHidingIntelligent },
                    ]}
                    onChange={(mode) => update({ autoHide: mode === "automatic", smartHide: mode === "intelligent" })}
                  />
                </Row>
                <SubRow label={t.dockAutoHideSpeed} open={config.autoHide || (config.smartHide ?? false)}>
                  <Segmented<DockAutoHideSpeed>
                    label={t.dockAutoHideSpeed}
                    value={config.dockAutoHideSpeed ?? "normal"}
                    options={[
                      { value: "smooth", label: t.dockAutoHideSmooth },
                      { value: "normal", label: t.dockDensityNormal },
                      { value: "fast", label: t.genieSpeedFast },
                    ]}
                    onChange={(dockAutoHideSpeed) => update({ dockAutoHideSpeed })}
                  />
                </SubRow>
                <Row icon={<SettingIcon name="bounce" />} label={t.bounceOnOpen}>
                  <Toggle
                    label={t.bounceOnOpen}
                    checked={config.bounceOnOpen}
                    onChange={(bounceOnOpen) => update({ bounceOnOpen })}
                  />
                </Row>
                <Row icon={<SettingIcon name="mascot" />} label={t.mascot} hint={t.mascotHint}>
                  <Toggle
                    label={t.mascot}
                    checked={config.mascot}
                    onChange={(mascot) => update({ mascot })}
                  />
                </Row>
                <SubRow label={t.mascotCharacter} stacked open={config.mascot}>
                  <VisualChoices<MascotCharacter>
                    label={t.mascotCharacter}
                    hideLabel
                    columns={3}
                    value={config.mascotCharacter ?? "crab"}
                    options={[
                      { value: "crab", label: t.mascotCrab, preview: <MascotPreview character="crab" /> },
                      { value: "cat", label: t.mascotCat, preview: <MascotPreview character="cat" /> },
                      { value: "fox", label: t.mascotFox, preview: <MascotPreview character="fox" /> },
                      { value: "capybara", label: t.mascotCapybara, preview: <MascotPreview character="capybara" /> },
                      { value: "mochi", label: t.mascotMochi, preview: <MascotPreview character="mochi" /> },
                      { value: "girl", label: t.mascotGirl, preview: <MascotPreview character="girl" /> },
                    ]}
                    onChange={(mascotCharacter) => update({ mascotCharacter }, true)}
                    onReselect={(mascotCharacter) => update({ mascotCharacter }, true)}
                  />
                </SubRow>
                <SubRow label={t.mascotAlwaysVisible} hint={t.mascotAlwaysVisibleHint} open={config.mascot}>
                  <Toggle
                    label={t.mascotAlwaysVisible}
                    checked={config.mascotAlwaysVisible ?? false}
                    onChange={(mascotAlwaysVisible) => update({ mascotAlwaysVisible })}
                  />
                </SubRow>
                <SubRow label={t.mascotBreak} hint={t.mascotBreakHint} open={config.mascot}>
                  <Toggle
                    label={t.mascotBreak}
                    checked={config.mascotBreakReminder ?? false}
                    onChange={(mascotBreakReminder) => update({ mascotBreakReminder })}
                  />
                </SubRow>
                <Row icon={<SettingIcon name="indicators" />} label={t.showIndicators}>
                  <Toggle
                    label={t.showIndicators}
                    checked={config.showIndicators}
                    onChange={(showIndicators) => update({ showIndicators })}
                  />
                </Row>
                <Row icon={<SettingIcon name="recents" />} label={t.showRecents} hint={t.showRecentsHint}>
                  <Toggle
                    label={t.showRecents}
                    checked={config.showRecents}
                    onChange={(showRecents) => update({ showRecents })}
                  />
                </Row>
                <Row icon={<SettingIcon name="jumpList" />} label={t.jumpLists} hint={t.jumpListsHint}>
                  <Toggle
                    label={t.jumpLists}
                    checked={config.jumpLists}
                    onChange={(jumpLists) => update({ jumpLists })}
                  />
                </Row>
              </Card>
              <WeatherSettings config={config} texts={t} onChange={update} />
              <Card title={t.dockItems}>
                <Row label={t.showStart}>
                  <Toggle label={t.showStart} checked={config.showStart ?? true} onChange={(showStart) => update({ showStart })} />
                </Row>
                <Row label={t.pinExplorer} hint={t.pinExplorerHint}>
                  <Toggle label={t.pinExplorer} checked={config.pinExplorer ?? true} onChange={(pinExplorer) => update({ pinExplorer })} />
                </Row>
                <Row label={t.showDownloads}>
                  <Toggle label={t.showDownloads} checked={config.showDownloads ?? true} onChange={(showDownloads) => update({ showDownloads })} />
                </Row>
                <Row label={t.showTrash}>
                  <Toggle label={t.showTrash} checked={config.showTrash ?? true} onChange={(showTrash) => update({ showTrash })} />
                </Row>
              </Card>
              <Card>
                <Row icon={<SettingIcon name="folder" />} label={t.shortcutsFolder} hint={config.shortcutsFolder ?? t.shortcutsFolderHint}>
                  <div className="push-group">
                    {config.shortcutsFolder ? (
                      <button type="button" className="push" onClick={() => update({ shortcutsFolder: null })}>
                        {t.shortcutsFolderRemove}
                      </button>
                    ) : null}
                    <button type="button" className="push" onClick={chooseFolder}>
                      {t.shortcutsFolderChoose}
                    </button>
                  </div>
                </Row>
              </Card>
            </>
          ) : section === "windows" ? (
            <>
              <Card title={t.groupMinimize}>
                <Row icon={<SettingIcon name="genie" />} label={t.genie} hint={t.genieHint}>
                  <Toggle
                    label={t.genie}
                    checked={config.genie}
                    onChange={(genie) => update({ genie })}
                  />
                </Row>
                <SubRow label={t.genieSpeed} open={config.genie} stacked>
                  <VisualChoices<GenieSpeed>
                    label={t.genieSpeed}
                    hideLabel
                    value={config.genieSpeed}
                    options={[
                      { value: "normal", label: t.genieSpeedNormal, preview: <SpeedPreview speed="normal" />, detail: `${durationFormat.format(0.5)} s` },
                      { value: "fast", label: t.genieSpeedFast, preview: <SpeedPreview speed="fast" />, detail: `${durationFormat.format(0.35)} s` },
                      { value: "faster", label: t.genieSpeedFaster, preview: <SpeedPreview speed="faster" />, detail: `${durationFormat.format(0.25)} s` },
                    ]}
                    onChange={(genieSpeed) => update({ genieSpeed })}
                  />
                </SubRow>
                <SubRow label={t.genieOff} hint={t.genieOffHint} open={config.genie && !animations}>
                  <button type="button" className="push" onClick={() => void openAnimationSettings()}>
                    {t.genieOffAction}
                  </button>
                </SubRow>
                <Row icon={<SettingIcon name="windows" />} label={t.showMinimized} hint={t.showMinimizedHint}>
                  <Toggle
                    label={t.showMinimized}
                    checked={config.showMinimized}
                    onChange={(showMinimized) => update({ showMinimized })}
                  />
                </Row>
              </Card>
              <Card>
                <Row label={t.showOtherWorkspaces} hint={t.showOtherWorkspacesHint}>
                  <Toggle
                    label={t.showOtherWorkspaces}
                    checked={config.showOtherWorkspaces ?? false}
                    onChange={(showOtherWorkspaces) => update({ showOtherWorkspaces })}
                  />
                </Row>
              </Card>
            </>
          ) : section === "menubar" ? (
            <>
              <Card>
                <Row icon={<SettingIcon name="menu" />} label={t.menuBarShow} hint={t.menuBarHint}>
                  <Toggle
                    label={t.menuBarShow}
                    checked={config.menuBar}
                    onChange={(menuBar) => update({ menuBar })}
                  />
                </Row>
                <SubRow
                  label={t.menuBarHidingMode}
                  hint={config.menuBarAutoHide ? t.menuBarAutoHideHint : config.menuBarSmartHide ? t.menuBarHidingIntelligentHint : t.menuBarHidingVisibleHint}
                  open={config.menuBar}
                >
                  <Select<DockHidingMode>
                    label={t.menuBarHidingMode}
                    value={config.menuBarAutoHide ? "automatic" : config.menuBarSmartHide ? "intelligent" : "visible"}
                    options={[
                      { value: "visible", label: t.dockHidingVisible },
                      { value: "automatic", label: t.dockHidingAutomatic },
                      { value: "intelligent", label: t.dockHidingIntelligent },
                    ]}
                    onChange={(mode) => update({ menuBarAutoHide: mode === "automatic", menuBarSmartHide: mode === "intelligent" })}
                  />
                </SubRow>
                <SubRow label={t.menuBarAllDisplays} hint={t.menuBarAllDisplaysHint} open={config.menuBar}>
                  <Toggle
                    label={t.menuBarAllDisplays}
                    checked={config.menuBarAllDisplays ?? true}
                    onChange={(menuBarAllDisplays) => update({ menuBarAllDisplays })}
                  />
                </SubRow>
                <SubRow label={t.menuBarMusic} hint={t.menuBarMusicHint} open={config.menuBar}>
                  <Toggle
                    label={t.menuBarMusic}
                    checked={config.menuBarMusic}
                    onChange={(menuBarMusic) => update({ menuBarMusic })}
                  />
                </SubRow>
                <SubRow label={t.aiUsage} hint={t.aiUsageHint} open={config.menuBar}>
                  <Toggle
                    label={t.aiUsage}
                    checked={config.aiUsage}
                    onChange={(aiUsage) => update({ aiUsage })}
                  />
                </SubRow>
                <SubRow
                  label={t.islandStyle}
                  stacked
                  open={config.menuBar && (config.menuBarMusic || config.aiUsage)}
                >
                  <VisualChoices<IslandStyle>
                    label={t.islandStyle}
                    hideLabel
                    value={config.menuBarIslandStyle}
                    options={[
                      { value: "notch", label: t.islandStyleNotch, preview: <IslandPreview style="notch" wallpaper={wallpaper} /> },
                      { value: "bubble", label: t.islandStyleBubble, preview: <IslandPreview style="bubble" wallpaper={wallpaper} /> },
                    ]}
                    onChange={(menuBarIslandStyle) => update({ menuBarIslandStyle })}
                  />
                </SubRow>
                <SubRow label={t.accessoryBattery} hint={t.accessoryBatteryHint} open={config.menuBar}>
                  <Toggle
                    label={t.accessoryBattery}
                    checked={config.accessoryBattery}
                    onChange={(accessoryBattery) => update({ accessoryBattery })}
                  />
                </SubRow>
                <SubRow label={t.menuBarSystemMonitor} hint={t.menuBarSystemMonitorHint} open={config.menuBar}>
                  <Toggle
                    label={t.menuBarSystemMonitor}
                    checked={config.menuBarSystemMonitor ?? false}
                    onChange={(menuBarSystemMonitor) => update({ menuBarSystemMonitor })}
                  />
                </SubRow>
                <SubRow label={t.menuBarNotifications} hint={t.menuBarNotificationsHint} open={config.menuBar}>
                  <Toggle
                    label={t.menuBarNotifications}
                    checked={config.menuBarNotifications ?? false}
                    onChange={(menuBarNotifications) => update({ menuBarNotifications })}
                  />
                </SubRow>
                <SubRow label={t.clockTarget} hint={config.menuBarClockTarget ?? ((config.calendar ?? true) ? t.clockTargetCalendar : t.clockTargetDefault)} open={config.menuBar}>
                  <div className="push-group">
                    {config.menuBarClockTarget ? (
                      <button type="button" className="push" onClick={() => update({ menuBarClockTarget: null })}>
                        {t.clockTargetReset}
                      </button>
                    ) : null}
                    <button type="button" className="push" onClick={chooseClockTarget}>
                      {t.clockTargetChoose}
                    </button>
                  </div>
                </SubRow>
              </Card>
              {config.menuBar ? (
                <>
                  <Card title={t.menuBarExtras}>
                    <Row label={t.menuBarCircle}>
                      <Toggle
                        label={t.menuBarCircle}
                        checked={config.menuBarCircle ?? true}
                        onChange={(menuBarCircle) => update({ menuBarCircle })}
                      />
                    </Row>
                    <SubRow label={t.menuBarIconStyle} stacked open={iconShown}>
                      <VisualChoices<MenuBarIcon>
                        label={t.menuBarIconStyle}
                        hideLabel
                        value={menuIcon}
                        options={[
                          { value: "ring", label: t.menuBarIconRing, preview: <MenuIconPreview icon="ring" /> },
                          { value: "windows", label: t.menuBarIconWindows, preview: <MenuIconPreview icon="windows" /> },
                          { value: "custom", label: t.menuBarIconCustom, preview: <MenuIconPreview icon="custom" picture={iconPreview} /> },
                        ]}
                        onChange={(menuBarIcon) => { setIconProblem(null); update({ menuBarIcon }); }}
                      />
                    </SubRow>
                    <SubRow
                      label={t.menuBarIconFile}
                      hint={iconProblem ? `${t[ICON_PROBLEM_TEXTS[iconProblem]]} ${t.menuBarIconRules}` : t.menuBarIconRules}
                      open={iconShown && menuIcon === "custom"}
                    >
                      <div className="push-group">
                        {config.menuBarIconFile ? (
                          <button type="button" className="push" onClick={() => { setIconProblem(null); update({ menuBarIconFile: null }); }}>
                            {t.menuBarIconRemove}
                          </button>
                        ) : null}
                        <button type="button" className="push" onClick={chooseMenuIcon}>
                          {t.menuBarIconChoose}
                        </button>
                      </div>
                    </SubRow>
                    <SubRow label={t.menuBarIconAnimation} hint={t.menuBarIconAnimationHint} open={iconShown}>
                      <Toggle
                        label={t.menuBarIconAnimation}
                        checked={config.menuBarIconAnimation ?? true}
                        onChange={(menuBarIconAnimation) => update({ menuBarIconAnimation })}
                      />
                    </SubRow>
                    <SubRow label={t.menuBarCircleMenu} hint={t.menuBarCircleMenuHint} open={config.menuBarCircle ?? true}>
                      <Toggle
                        label={t.menuBarCircleMenu}
                        checked={config.menuBarCircleMenu ?? true}
                        onChange={(menuBarCircleMenu) => update({ menuBarCircleMenu })}
                      />
                    </SubRow>
                    <Row label={t.trayMenu} hint={t.trayMenuHint}>
                      <Toggle
                        label={t.trayMenu}
                        checked={config.trayMenu ?? true}
                        onChange={(trayMenu) => update({ trayMenu })}
                      />
                    </Row>
                    <Row label={t.menuBarDynamicArrow} hint={t.menuBarDynamicArrowHint}>
                      <Toggle
                        label={t.menuBarDynamicArrow}
                        checked={config.menuBarDynamicArrow ?? true}
                        onChange={(menuBarDynamicArrow) => update({ menuBarDynamicArrow })}
                      />
                    </Row>
                    <Row label={t.networkMenu} hint={t.networkMenuHint}>
                      <Toggle
                        label={t.networkMenu}
                        checked={config.networkMenu ?? true}
                        onChange={(networkMenu) => update({ networkMenu })}
                      />
                    </Row>
                    <Row label={t.menuBarCalendar} hint={t.menuBarCalendarHint}>
                      <Toggle
                        label={t.menuBarCalendar}
                        checked={config.calendar ?? true}
                        onChange={(calendar) => update({ calendar })}
                      />
                    </Row>
                    <SubRow label={t.weekStart} open={config.calendar ?? true}>
                      <Select<WeekStart>
                        label={t.weekStart}
                        value={config.calendarWeekStart ?? "auto"}
                        options={[
                          { value: "auto", label: t.weekStartAuto },
                          { value: "monday", label: t.weekStartMonday },
                          { value: "sunday", label: t.weekStartSunday },
                          { value: "saturday", label: t.weekStartSaturday },
                        ]}
                        onChange={(calendarWeekStart) => update({ calendarWeekStart })}
                      />
                    </SubRow>
                  </Card>
                  <Card title={t.controlCenterGroup}>
                    <Row label={t.controlCenter} hint={t.controlCenterHint}>
                      <Toggle
                        label={t.controlCenter}
                        checked={config.controlCenter ?? true}
                        onChange={(controlCenter) => update({ controlCenter })}
                      />
                    </Row>
                    {CONTROL_TILES.map((tile) => (
                      <SubRow key={tile} label={t[TILE_LABELS[tile]]} open={config.controlCenter ?? true}>
                        <Toggle
                          label={t[TILE_LABELS[tile]]}
                          checked={config.controlCenterTiles?.[tile] ?? true}
                          onChange={(shown) => update({
                            controlCenterTiles: { ...(config.controlCenterTiles ?? ALL_TILES), [tile]: shown },
                          })}
                        />
                      </SubRow>
                    ))}
                  </Card>
                </>
              ) : null}
            </>
          ) : section === "search" ? (
            <SearchSection
              t={t}
              search={config.search}
              onChange={(patch) => update({ search: { ...config.search, ...patch } })}
              flush={flushSettings}
            />
          ) : (
            <>
              <Card>
                <Row icon={<SettingIcon name="start" />} label={t.autostart}>
                  <Toggle label={t.autostart} checked={autostart} onChange={changeAutostart} />
                </Row>
                <Row icon={<SettingIcon name="language" />} label={t.language}>
                  <Select<Language>
                    label={t.language}
                    value={config.language}
                    options={[
                      { value: "auto", label: t.languageAuto },
                      ...LANGUAGE_OPTIONS,
                    ]}
                    onChange={(language) => update({ language })}
                  />
                </Row>
              </Card>
              <Card title={t.updates}>
                <Row icon={<SettingIcon name="updates" />} label={t.checkUpdates} hint={t.checkUpdatesHint}>
                  <Toggle
                    label={t.checkUpdates}
                    checked={config.checkUpdates}
                    onChange={(checkUpdates) => update({ checkUpdates })}
                  />
                </Row>
                <SubRow label={t.autoUpdate} hint={t.autoUpdateHint} open={config.checkUpdates}>
                  <Toggle
                    label={t.autoUpdate}
                    checked={config.autoUpdate}
                    onChange={(autoUpdate) => update({ autoUpdate })}
                  />
                </SubRow>
                <Row icon={<SettingIcon name="updates" />} label={t.updatesNow} hint={updateHint(t, updateState, version)}>
                  {waiting && updateState.kind !== "installing" ? (
                    <button type="button" className="push push-primary" onClick={() => install(waiting)}>
                      {t.updatesInstall}
                    </button>
                  ) : (
                    <button
                      type="button"
                      className="push"
                      disabled={updateState.kind === "checking" || updateState.kind === "installing"}
                      onClick={recheck}
                    >
                      {t.updatesCheck}
                    </button>
                  )}
                </Row>
              </Card>
              <Card>
                <Row icon={<SettingIcon name="reset" />} label={t.reset} hint={confirmingReset ? t.resetConfirmHint : t.resetHint}>
                  <button
                    type="button"
                    className={confirmingReset ? "push push-danger" : "push"}
                    onClick={reset}
                  >
                    {confirmingReset ? t.resetConfirm : t.resetButton}
                  </button>
                </Row>
              </Card>
            </>
          )}
          {failed ? (
            <p className="error" role="alert">
              {t.saveError}
            </p>
          ) : null}
        </div>
      </main>
    </div>
  );
}

function updateHint(t: Texts, state: UpdateState, version: string): string {
  switch (state.kind) {
    case "off":
      return t.updatesOff;
    case "checking":
      return t.updatesChecking;
    case "current":
      return t.updatesCurrent.replace("{version}", version);
    case "available":
      return t.updatesAvailable.replace("{version}", state.info.version);
    case "installing":
      return t.updatesInstalling;
    case "installFailed":
      return t.updateInstallError;
    case "error":
      return t.updatesError;
  }
}

const SECTIONS: readonly SectionId[] = ["appearance", "dock", "windows", "menubar", "search", "system"];

const ALL_TILES = Object.fromEntries(CONTROL_TILES.map((tile) => [tile, true])) as ControlTiles;

const ICON_PROBLEMS: readonly IconProblem[] = ["format", "size", "square", "dimensions", "save"];
/** What each refusal says. */
const ICON_PROBLEM_TEXTS = {
  format: "menuBarIconBadFormat",
  size: "menuBarIconTooBig",
  square: "menuBarIconNotSquare",
  dimensions: "menuBarIconBadSize",
  save: "menuBarIconSaveFailed",
} as const satisfies Record<IconProblem, keyof Texts>;

/** Each tile's name among the texts. */
const TILE_LABELS = {
  network: "tileNetwork",
  media: "tileMedia",
  bluetooth: "tileBluetooth",
  nearbySharing: "tileNearbySharing",
  focus: "tileFocus",
  taskView: "tileTaskView",
  cast: "tileCast",
  display: "tileDisplay",
  sound: "tileSound",
  darkMode: "tileDarkMode",
  calculator: "tileCalculator",
  timer: "tileTimer",
  screenshot: "tileScreenshot",
} as const satisfies Record<keyof ControlTiles, keyof Texts>;

const THEMES: readonly Appearance[] = ["auto", "light", "dark", "frosted", "clear", "liquidGlass"];

function themeName(t: Texts, theme: Appearance): string {
  switch (theme) {
    case "auto":
      return t.themeAuto;
    case "light":
      return t.themeLight;
    case "dark":
      return t.themeDark;
    case "frosted":
      return t.themeFrosted;
    case "clear":
      return t.themeClear;
    case "liquidGlass":
      return t.themeLiquidGlass;
  }
}

/** Material presets style the shell; only explicit light/dark override Windows' app theme. */
const windowTheme = (theme: Appearance) =>
  theme === "light" || theme === "dark" ? theme : null;

function sectionTitle(t: Texts, section: SectionId): string {
  switch (section) {
    case "appearance":
      return t.appearance;
    case "dock":
      return t.dock;
    case "windows":
      return t.windows;
    case "menubar":
      return t.menuBar;
    case "search":
      return t.search;
    case "system":
      return t.system;
  }
}

interface NavItemProps {
  section: SectionId;
  active: boolean;
  onSelect: () => void;
  /** Accessible text of a red badge, as macOS shows for a pending update. */
  badge?: string;
  children: string;
}

function NavItem({ section, active, onSelect, badge, children }: NavItemProps) {
  return (
    <button
      type="button"
      className="nav-item"
      aria-current={active ? "page" : undefined}
      onClick={onSelect}
    >
      <SectionIcon section={section} />
      {children}
      {badge ? (
        <span className="nav-badge" role="img" aria-label={badge}>
          1
        </span>
      ) : null}
    </button>
  );
}
