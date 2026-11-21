import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  type Config,
  type DockPosition,
  type GenieSpeed,
  type IslandStyle,
  type Language,
  type MenuBarItem,
  effectiveMenuBarOrder,
  type UpdateInfo,
  type WindowsPreferences,
  ICON_SIZE,
  MAX_MAGNIFIED_SIZE,
  checkUpdate,
  defaultConfig,
  getAutostart,
  openAnimationSettings,
  loadWindowsPreferences,
  installUpdate,
  loadConfig,
  loadWallpaperPreview,
  openReleaseNotes,
  openRepository,
  pickFolder,
  saveConfig,
  setAutostart,
} from "./config";
import { Card, Row, Segmented, Select, Slider, SubRow, Toggle, VisualChoices } from "./controls";
import { DockPreview, IslandPreview, SettingIcon, SpeedPreview } from "./visuals";
import { type SectionId, SectionIcon } from "./icons";
import { UpdateBanner } from "./UpdateBanner";
import en from "./i18n/en.json";
import es from "./i18n/es.json";
import fr from "./i18n/fr.json";

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
type Texts = typeof es;

/** Dragging a slider saves at most this often; the dock redraws on each save. */
const SAVE_DELAY_MS = 60;
/** How long the reset button stays armed after the first click. */
const RESET_CONFIRM_MS = 4000;

const TEXTS: Record<Exclude<Language, "auto">, Texts> = { es, en, fr };

function textsFor(language: Language): Texts {
  if (language !== "auto") {
    return TEXTS[language];
  }
  // Follow the Windows display language, English when there is no text in it.
  const windows = navigator.language.toLowerCase().slice(0, 2);
  return windows === "es" || windows === "fr" ? TEXTS[windows] : en;
}

export function App() {
  const [config, setConfig] = useState<Config | null>(null);
  const [autostart, setAutostartState] = useState(false);
  const [windowsPreferences, setWindowsPreferences] = useState<WindowsPreferences | null>(null);
  // Native drag and drop of the menu bar items: the one held and the one it is over.
  const [draggedItem, setDraggedItem] = useState<MenuBarItem | null>(null);
  const [dropItem, setDropItem] = useState<MenuBarItem | null>(null);
  // Keep the animation notice from flashing while the initial system read is pending.
  const animations = windowsPreferences?.animations ?? true;
  useEffect(() => {
    let alive = true;
    const check = () => {
      loadWindowsPreferences()
        .then((values) => { if (alive) setWindowsPreferences(values); })
        .catch(() => {});
    };
    check();
    // One shared focus listener, with no polling while Settings is idle or closed.
    window.addEventListener("focus", check);
    return () => { alive = false; window.removeEventListener("focus", check); };
  }, []);
  const [section, setSection] = useState<SectionId>("dock");
  const [failed, setFailed] = useState(false);
  const [version, setVersion] = useState("");
  const [updateState, setUpdateState] = useState<UpdateState>({ kind: "checking" });
  const [confirmingReset, setConfirmingReset] = useState(false);
  const [projectError, setProjectError] = useState(false);
  const [wallpaper, setWallpaper] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    loadWallpaperPreview().then((image) => { if (alive) setWallpaper(image); });
    return () => { alive = false; };
  }, []);
  /** "Later" hides the banner until Settings is opened again. */
  const [bannerDismissed, setBannerDismissed] = useState(false);
  const pending = useRef<Partial<Config> | null>(null);
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
        try {
          await saveConfig(latest);
          setFailed(false);
          return true;
        } catch {
          pending.current = Object.assign({}, latest, pending.current);
          setFailed(true);
          return false;
        }
      });
    }
    return writes.current;
  }, []);

  useEffect(() => {
    let alive = true;
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
    getVersion()
      .then((current) => alive && setVersion(current))
      .catch(() => {});
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
      window.clearTimeout(timer.current);
      window.clearTimeout(resetTimer.current);
      progress.then((unlisten) => unlisten()).catch(() => {});
      close.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [flushSettings]);

  // Shows the change at once and saves the latest value shortly after, so a
  // slider drag does not write the file on every pixel.
  const update = useCallback((patch: Partial<Config>) => {
    setConfig((current) => {
      if (!current) return current;
      const next = { ...current, ...patch };
      if (next.magnifiedSize < next.iconSize) next.magnifiedSize = next.iconSize;
      return next;
    });
    pending.current = { ...pending.current, ...patch };
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
    if (info.notes) openReleaseNotes(info.notes).catch(() => {});
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
        update(defaults),
      )
      .catch(() => setFailed(true));
  }, [confirmingReset, update]);

  if (!config) return <div className="window" />;
  const t = textsFor(config.language);
  const menuOrder = effectiveMenuBarOrder(config.menuBarOrder);
  const menuHidden = config.menuBarHidden ?? [];
  const moveMenuItem = (item: MenuBarItem, to: number) => {
    const from = menuOrder.indexOf(item);
    if (from < 0 || to < 0 || to >= menuOrder.length || from === to) return;
    const next = [...menuOrder];
    next.splice(from, 1);
    next.splice(to, 0, item);
    update({ menuBarOrder: next });
  };
  const menuItemShown = (item: MenuBarItem): boolean | undefined => {
    switch (item) {
      case "bluetooth":
        return config.menuBarBluetooth;
      case "search":
        return config.menuBarSearch ?? windowsPreferences?.menuBarSearch;
      case "task-view":
        return config.menuBarTaskView ?? windowsPreferences?.menuBarTaskView;
      case "widgets":
        return config.menuBarWidgets ?? windowsPreferences?.menuBarWidgets;
      default:
        return !menuHidden.includes(item);
    }
  };
  const showMenuItem = (item: MenuBarItem, shown: boolean) => {
    switch (item) {
      case "bluetooth":
        return update({ menuBarBluetooth: shown });
      case "search":
        return update({ menuBarSearch: shown });
      case "task-view":
        return update({ menuBarTaskView: shown });
      case "widgets":
        return update({ menuBarWidgets: shown });
      default:
        return update({
          menuBarHidden: shown ? menuHidden.filter((hidden) => hidden !== item) : [...new Set([...menuHidden, item])],
        });
    }
  };
  const durationFormat = new Intl.NumberFormat(config.language === "auto" ? navigator.language : config.language);
  const chooseFolder = () => {
    pickFolder(config.shortcutsFolder)
      .then((folder) => folder && update({ shortcutsFolder: folder }))
      .catch(() => setFailed(true));
  };
  const appWindow = getCurrentWindow();
  const waiting = pendingUpdate(updateState);
  // An update the user asked for keeps its banner even after "Later".
  const showBanner =
    waiting !== null && (!bannerDismissed || updateState.kind !== "available");

  return (
    <div className="window">
      <aside className="sidebar" data-tauri-drag-region>
        <div className="traffic">
          <button
            type="button"
            className="light light-close"
            aria-label={t.close}
            onClick={() => appWindow.close()}
          />
          <button
            type="button"
            className="light light-minimize"
            aria-label={t.minimize}
            onClick={() => appWindow.minimize()}
          />
          <button
            type="button"
            className="light light-zoom"
            aria-label={t.zoom}
            onClick={() => appWindow.toggleMaximize()}
          />
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
          {section === "dock" ? (
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
                <Row icon={<SettingIcon name="hide" />} label={t.autoHide} hint={t.autoHideHint}>
                  <Toggle
                    label={t.autoHide}
                    checked={config.autoHide}
                    onChange={(autoHide) => update({ autoHide })}
                  />
                </Row>
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
                <Row label={t.showPreviewCloseButton} hint={t.showPreviewCloseButtonHint}>
                  <Toggle
                    label={t.showPreviewCloseButton}
                    checked={config.showPreviewCloseButton}
                    onChange={(showPreviewCloseButton) => update({ showPreviewCloseButton })}
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
                <SubRow label={t.menuBarAutoHide} hint={t.menuBarAutoHideHint} open={config.menuBar}>
                  <Toggle
                    label={t.menuBarAutoHide}
                    checked={config.menuBarAutoHide}
                    onChange={(menuBarAutoHide) => update({ menuBarAutoHide })}
                  />
                </SubRow>
                <SubRow label={t.menuBarGlass} hint={t.menuBarGlassHint} open={config.menuBar}>
                  <Toggle
                    label={t.menuBarGlass}
                    checked={config.menuBarGlass}
                    onChange={(menuBarGlass) => update({ menuBarGlass })}
                  />
                </SubRow>
                <SubRow
                  label={t.menuBarGlassStrength}
                  hint={`${t.menuBarGlassClear} … ${t.menuBarGlassFrosted}`}
                  open={config.menuBar && config.menuBarGlass}
                >
                  <Slider
                    label={t.menuBarGlassStrength}
                    value={config.menuBarGlassStrength ?? 50}
                    min={0}
                    max={100}
                    step={5}
                    unit="%"
                    onChange={(menuBarGlassStrength) => update({ menuBarGlassStrength })}
                  />
                </SubRow>
                <SubRow label={t.menuBarMusic}hint={t.menuBarMusicHint} open={config.menuBar}>
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
              </Card>
              {config.menuBar && (
                <Card title={t.clock}>
                  <Row label={t.clockShowDate}>
                    <Toggle
                      label={t.clockShowDate}
                      checked={config.clockShowDate ?? true}
                      onChange={(clockShowDate) => update({ clockShowDate })}
                    />
                  </Row>
                  <Row label={t.clockShowWeekday}>
                    <Toggle
                      label={t.clockShowWeekday}
                      checked={config.clockShowWeekday ?? true}
                      onChange={(clockShowWeekday) => update({ clockShowWeekday })}
                    />
                  </Row>
                  <Row label={t.clockSeconds}>
                    <Toggle
                      label={t.clockSeconds}
                      checked={config.clockSeconds ?? false}
                      onChange={(clockSeconds) => update({ clockSeconds })}
                    />
                  </Row>
                  <Row label={t.clock24h}>
                    <Select<"auto" | "on" | "off">
                      label={t.clock24h}
                      value={config.clock24h === true ? "on" : config.clock24h === false ? "off" : "auto"}
                      options={[
                        { value: "auto", label: t.clock24hAuto },
                        { value: "on", label: t.clock24hOn },
                        { value: "off", label: t.clock24hOff },
                      ]}
                      onChange={(choice) => update({ clock24h: choice === "auto" ? null : choice === "on" })}
                    />
                  </Row>
                </Card>
              )}
              {config.menuBar && (
                <Card title={t.menuBarItems}>
                  <p className="list-hint">{t.menuBarItemsHint}</p>
                  <ul className="reorder-list">
                    {menuOrder.map((item, index) => {
                      const label = menuItemLabel(t, item);
                      return (
                        <li
                          key={item}
                          className="row reorder-row"
                          draggable
                          data-dragging={draggedItem === item}
                          data-over={dropItem === item && draggedItem !== item}
                          onDragStart={(event) => {
                            event.dataTransfer.effectAllowed = "move";
                            event.dataTransfer.setData("text/plain", item);
                            setDraggedItem(item);
                          }}
                          onDragOver={(event) => {
                            if (!draggedItem) return;
                            event.preventDefault();
                            event.dataTransfer.dropEffect = "move";
                            if (dropItem !== item) setDropItem(item);
                          }}
                          onDrop={(event) => {
                            event.preventDefault();
                            if (draggedItem) moveMenuItem(draggedItem, index);
                            setDraggedItem(null);
                            setDropItem(null);
                          }}
                          onDragEnd={() => {
                            setDraggedItem(null);
                            setDropItem(null);
                          }}
                        >
                          <span className="reorder-grip" title={t.menuBarDrag} aria-hidden="true">
                            <SettingIcon name="grip" />
                          </span>
                          <span className="row-icon"><SettingIcon name={MENU_ITEM_ICONS[item]} /></span>
                          <div className="row-text">
                            <span className="row-label">{label}</span>
                          </div>
                          <div className="reorder-buttons">
                            <button
                              type="button"
                              className="reorder-button"
                              aria-label={`${t.menuBarMoveUp}: ${label}`}
                              disabled={index === 0}
                              onClick={() => moveMenuItem(item, index - 1)}
                            >
                              <SettingIcon name="up" size={14} />
                            </button>
                            <button
                              type="button"
                              className="reorder-button"
                              aria-label={`${t.menuBarMoveDown}: ${label}`}
                              disabled={index === menuOrder.length - 1}
                              onClick={() => moveMenuItem(item, index + 1)}
                            >
                              <SettingIcon name="down" size={14} />
                            </button>
                          </div>
                          <div className="row-control">
                            <Toggle
                              label={label}
                              checked={menuItemShown(item)}
                              onChange={(shown) => showMenuItem(item, shown)}
                            />
                          </div>
                        </li>
                      );
                    })}
                  </ul>
                  <Row icon={<SettingIcon name="battery" />} label={t.menuBarBatteryPercent} hint={t.menuBarBatteryPercentHint}>
                    <Toggle
                      label={t.menuBarBatteryPercent}
                      checked={config.menuBarBatteryPercent ?? windowsPreferences?.menuBarBatteryPercent}
                      onChange={(menuBarBatteryPercent) => update({ menuBarBatteryPercent })}
                    />
                  </Row>
                  <Row icon={<SettingIcon name="tray" />} label={t.menuBarTrayPopup} hint={t.menuBarTrayPopupHint}>
                    <Toggle
                      label={t.menuBarTrayPopup}
                      checked={config.menuBarTrayPopup}
                      onChange={(menuBarTrayPopup) => update({ menuBarTrayPopup })}
                    />
                  </Row>
                </Card>
              )}
            </>
          ) : section === "notifications" ? (
            <>
              <Card>
                <Row label={t.notificationsEnabled} hint={t.notificationsHint}>
                  <Toggle
                    label={t.notificationsEnabled}
                    checked={config.notifications}
                    onChange={(notifications) => update({ notifications })}
                  />
                </Row>
                <SubRow label={t.notificationPosition} open={config.notifications}>
                  <Segmented
                    label={t.notificationPosition}
                    value={config.notificationPosition}
                    options={[
                      { value: "top-right", label: t.notificationPositionTopRight },
                      { value: "bottom-right", label: t.notificationPositionBottomRight },
                    ]}
                    onChange={(notificationPosition) => update({ notificationPosition })}
                  />
                </SubRow>
                <SubRow label={t.notificationDuration} open={config.notifications}>
                  <Slider
                    label={t.notificationDuration}
                    min={2}
                    max={15}
                    value={config.notificationDuration}
                    unit="s"
                    onChange={(notificationDuration) => update({ notificationDuration })}
                  />
                </SubRow>
              </Card>
              <Card>
                <Row label={t.notificationBadges} hint={t.notificationBadgesHint}>
                  <Toggle
                    label={t.notificationBadges}
                    checked={config.notificationBadges}
                    onChange={(notificationBadges) => update({ notificationBadges })}
                  />
                </Row>
                <Row label={t.notificationBounce} hint={t.notificationBounceHint}>
                  <Toggle
                    label={t.notificationBounce}
                    checked={config.notificationBounce}
                    onChange={(notificationBounce) => update({ notificationBounce })}
                  />
                </Row>
                <Row label={t.notificationDnd} hint={t.notificationDndHint}>
                  <Toggle
                    label={t.notificationDnd}
                    checked={config.notificationDnd}
                    onChange={(notificationDnd) => update({ notificationDnd })}
                  />
                </Row>
              </Card>
            </>
          ) : (
            <>
              <Card>
                <Row icon={<SettingIcon name="start" />} label={t.autostart}>
                  <Toggle label={t.autostart} checked={autostart} onChange={changeAutostart} />
                </Row>
                <Row icon={<SettingIcon name="language" />} label={t.language}>
                  <Segmented<Language>
                    label={t.language}
                    value={config.language}
                    options={[
                      { value: "auto", label: t.languageAuto },
                      { value: "es", label: "Español" },
                      { value: "en", label: "English" },
                      { value: "fr", label: "Français" },
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

type MenuItemIcon = Parameters<typeof SettingIcon>[0]["name"];

const MENU_ITEM_ICONS: Record<MenuBarItem, MenuItemIcon> = {
  "control-center": "control",
  focus: "focus",
  wifi: "wifi",
  bluetooth: "bluetooth",
  sound: "sound",
  battery: "battery",
  "input-language": "keyboard",
  "hidden-icons": "tray",
  search: "search",
  "task-view": "windows",
  widgets: "widgets",
  privacy: "microphone",
};

function menuItemLabel(t: Texts, item: MenuBarItem): string {
  switch (item) {
    case "control-center":
      return t.itemControlCenter;
    case "focus":
      return t.itemFocus;
    case "wifi":
      return t.itemWifi;
    case "bluetooth":
      return t.menuBarBluetooth;
    case "sound":
      return t.itemSound;
    case "battery":
      return t.itemBattery;
    case "input-language":
      return t.itemInputLanguage;
    case "hidden-icons":
      return t.itemHiddenIcons;
    case "search":
      return t.menuBarSearch;
    case "task-view":
      return t.menuBarTaskView;
    case "widgets":
      return t.menuBarWidgets;
    case "privacy":
      return t.itemPrivacy;
  }
}

const SECTIONS: readonly SectionId[] =["dock", "windows", "menubar", "notifications", "system"];

function sectionTitle(t: Texts, section: SectionId): string {
  switch (section) {
    case "dock":
      return t.dock;
    case "windows":
      return t.windows;
    case "menubar":
      return t.menuBar;
    case "notifications":
      return t.notifications;
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
