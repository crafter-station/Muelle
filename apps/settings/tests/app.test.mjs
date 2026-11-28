import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { runInThisContext } from "node:vm";
import test from "node:test";
import * as React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { transformWithEsbuild } from "vite";

// Exercise the actual App and controls without WebView, registry, network or timers.
// Compilation alone cannot catch a conditional accidentally rendered as JSX text.
const source = fileURLToPath(new URL("../src/", import.meta.url));
const wallpaperCount = JSON.parse(readFileSync(new URL("../wallpapers/catalog.json", import.meta.url), "utf8")).length;
const require = createRequire(import.meta.url);
const compiled = new Map();
for (const file of readdirSync(source).filter((name) => /\.tsx?$/.test(name))) {
  const filename = path.join(source, file);
  const { code } = await transformWithEsbuild(readFileSync(filename, "utf8"), filename, {
    format: "cjs", jsx: "automatic",
  });
  compiled.set(filename, runInThisContext(`(function(require,module,exports){${code}\n})`, { filename }));
}
const texts = Object.fromEntries(["en", "es", "fr", "zh", "hi", "ar", "bn", "pt"].map((language) => [
  language, JSON.parse(readFileSync(path.join(source, "i18n", `${language}.json`), "utf8")),
]));
const base = {
  language: "en", menuBar: true, smartHide: false,
  iconSize: 48, magnifiedSize: 77,
  magnification: true, dockPosition: "bottom", genie: true, genieSpeed: "normal",
  dockDensity: "normal", dockAutoHideSpeed: "normal", showStart: true,
  showTrash: true, pinExplorer: true,
  showDownloads: true,
  showOtherWorkspaces: false,
  menuBarIslandStyle: "notch", checkUpdates: false, appearance: "auto",
  pinned: [], recent: [], shortcutsFolder: null,
};
const settle = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
function* elements(node) {
  if (Array.isArray(node)) { for (const child of node) yield* elements(child); }
  else if (React.isValidElement(node)) { yield node; yield* elements(node.props.children); }
}

function fixture(section = "menubar", config = {}, save = async () => {}, galleryNative = {}) {
  const states = [], refs = [], effects = [], timers = new Map(), calls = [], listeners = new Map();
  let stateIndex = 0, refIndex = 0, nextTimer = 0, close;
  const loaded = { ...base, ...config };
  const gallery = section === "gallery";
  const weather = section === "weather";
  const weatherCalls = [];
  const wallpaperCalls = [], wallpaperPreviews = [], clockPicks = [], mascotPreviews = [];
  const browser = {
    addEventListener(event, callback) { assert.ok(!listeners.has(event)); listeners.set(event, callback); },
    removeEventListener(event) { listeners.delete(event); },
    setTimeout(callback, delay) { const id = ++nextTimer; timers.set(id, { callback, delay }); return id; },
    clearTimeout(id) { timers.delete(id); },
  };
  globalThis.window = browser;
  const hooks = {
    ...React,
    useState(initial) {
      const index = stateIndex++;
      if (!(index in states)) states[index] = !gallery && !weather && index === 0 ? { ...loaded } : !gallery && !weather && initial === "dock" ? section : initial;
      return [states[index], (value) => { states[index] = typeof value === "function" ? value(states[index]) : value; }];
    },
    useRef(initial) { const index = refIndex++; return refs[index] ??= { current: initial }; },
    useCallback(callback) { return callback; },
    useEffect(callback) { effects.push(callback); },
  };
  const nativeWindow = {
    themes: [],
    onCloseRequested(callback) { close = callback; return Promise.resolve(() => {}); },
    setTheme(theme) { this.themes.push(theme); return Promise.resolve(); },
  };
  const mocks = {
    "@tauri-apps/api/app": { getVersion: async () => "0.1.26" },
    "@tauri-apps/api/event": { listen: async () => () => {} },
    "@tauri-apps/api/window": { getCurrentWindow: () => nativeWindow },
    "@tauri-apps/api/core": { invoke: async (command, args) => {
      switch (command) {
        case "search_weather_cities": weatherCalls.push({ search: args.query, language: args.language }); return galleryNative.searchCities?.(args.query);
        case "locate_weather": weatherCalls.push("locate"); return galleryNative.locateWeather?.();
        case "cancel_weather_location": weatherCalls.push("cancel"); return;
        case "open_weather_location_settings": weatherCalls.push("privacy"); return;
        case "open_weather_attribution": weatherCalls.push("source"); return;
        case "get_config": return { ...loaded };
        case "get_autostart": return false;
        case "windows_preferences": return { animations: true };
        case "wallpaper_preview": return null;
        case "wallpaper_selection": return typeof galleryNative.selection === "function" ? galleryNative.selection() : galleryNative.selection ?? null;
        case "apply_wallpaper": wallpaperCalls.push({ ...args }); return galleryNative.apply?.(args.id);
        case "pick_clock_target": clockPicks.push({ ...args }); return galleryNative.clockTarget ?? null;
        case "default_config": return { ...base, autoHide: false, smartHide: true };
        case "set_config": calls.push({ ...args.patch }); mascotPreviews.push(args.previewMascot ?? false); return save(args.patch);
        default: throw new Error(`Unexpected native call: ${command}`);
      }
    } },
  };
  const modules = new Map();
  function load(filename) {
    if (modules.has(filename)) return modules.get(filename).exports;
    if (filename.endsWith(".json")) return JSON.parse(readFileSync(filename, "utf8"));
    if (filename.endsWith(".jpg")) return `file:///${filename.replaceAll("\\", "/")}`;
    const module = { exports: {} };
    modules.set(filename, module);
    compiled.get(filename)((id) => {
      if (id === "react" && (filename === path.join(source, "App.tsx") || gallery && filename === path.join(source, "WallpaperGallery.tsx") || weather && filename === path.join(source, "WeatherSettings.tsx"))) return hooks;
      if (id in mocks) return mocks[id];
      if (!id.startsWith(".")) return require(id);
      const resolved = path.resolve(path.dirname(filename), id);
      return load(compiled.has(`${resolved}.tsx`) ? `${resolved}.tsx` : compiled.has(`${resolved}.ts`) ? `${resolved}.ts` : resolved);
    }, module, module.exports);
    return module.exports;
  }
  const { App } = load(path.join(source, "App.tsx"));
  const { WallpaperGallery } = load(path.join(source, "WallpaperGallery.tsx"));
  const { WeatherSettings } = load(path.join(source, "WeatherSettings.tsx"));
  function render() {
    stateIndex = refIndex = 0; effects.length = 0;
    return gallery ? WallpaperGallery({ texts: texts[loaded.language], onApplied: (preview) => wallpaperPreviews.push(preview) })
      : weather ? WeatherSettings({ config: loaded, texts: texts[loaded.language], onChange: (patch) => { Object.assign(loaded, patch); calls.push(patch); } }) : App();
  }
  function find(predicate) { const node = [...elements(render())].find(predicate); assert.ok(node); return node; }
  return {
    calls, timers, render, themes: nativeWindow.themes, wallpaperCalls, wallpaperPreviews, clockPicks, mascotPreviews, weatherCalls,
    external(patch) { Object.assign(loaded, patch); },
    async focus() { listeners.get("focus")?.(); await settle(); },
    async mount() { render(); const cleanup = effects.map((effect) => effect()); await settle(); return () => cleanup.forEach((fn) => fn?.()); },
    change(label, value) { find((node) => node.props.label === label && typeof node.props.onChange === "function").props.onChange(value); },
    button(text) { find((node) => node.type === "button" && node.props.children === text).props.onClick(); },
    input(label, value) { find((node) => node.type === "input" && node.props["aria-label"] === label).props.onChange({ target: { value } }); },
    submit() { find((node) => node.type === "form").props.onSubmit({ preventDefault() {} }); },
    body() { return gallery || weather ? render() : find((node) => node.props.className === "content-body"); },
    fire(delay) { for (const [id, timer] of [...timers]) if (timer.delay === delay) { timers.delete(id); timer.callback(); } },
    async close() { let prevented = false; await close({ preventDefault() { prevented = true; } }); return prevented; },
  };
}

test("each section renders only its own controls, in every language and visibility state", () => {
  const markers = { appearance: "themeHint", dock: "size", windows: "genie", menubar: "menuBarShow", system: "autostart" };
  for (const language of Object.keys(texts)) for (const menuBar of [true, false]) {
    for (const section of Object.keys(markers)) {
      const view = fixture(section, { language, menuBar });
      const body = view.body();
      const html = renderToStaticMarkup(body);
      assert.doesNotMatch(html, /section\s*===|\)\s*:\s*section/);
      for (const [other, key] of Object.entries(markers)) {
        const escaped = renderToStaticMarkup(React.createElement("span", null, texts[language][key])).slice(6, -7);
        assert.equal(html.includes(escaped), other === section, `${language}/${section}: ${other}`);
      }
      assert.equal(body.key, section, "navigation resets the scroll container");
    }
  }
});

const wallpaperButtons = (view) => [...elements(view.body())].filter((node) => node.type === "button" && "data-wallpaper-id" in node.props);

test("the gallery bundles only offline previews and appears only inside Appearance", () => {
  for (const language of Object.keys(texts)) {
    assert.equal(texts[language].wallpaperNames.length, wallpaperCount);
    for (const section of ["appearance", "dock", "windows", "menubar", "system"]) {
      const view = fixture(section, { language });
      const html = renderToStaticMarkup(view.body());
      assert.equal((html.match(/class="wallpaper-choice"/g) ?? []).length, section === "appearance" ? wallpaperCount : 0);
      if (section === "appearance") {
        assert.equal((html.match(/loading="lazy"/g) ?? []).length, wallpaperCount);
        assert.doesNotMatch(html, /https?:\/\//, "opening the gallery never loads remote images");
      }
      assert.deepEqual(view.wallpaperCalls, [], "rendering must never change the desktop");
    }
  }
});

test("wallpapers follow the Windows selection and serialize rapid clicks until success", async () => {
  let finish;
  const view = fixture("gallery", {}, undefined, {
    selection: 3, apply: () => new Promise((resolve) => { finish = resolve; }),
  });
  const unmount = await view.mount();
  try {
    assert.equal(wallpaperButtons(view).find((button) => button.props["aria-pressed"]).props["data-wallpaper-id"], 3);
    const buttons = wallpaperButtons(view);
    const operation = buttons[3].props.onClick();
    await buttons[4].props.onClick(); // Simulate a second click before React repaints.
    assert.deepEqual(view.wallpaperCalls, [{ id: 4 }]);
    assert.ok(wallpaperButtons(view).every((button) => button.props.disabled));
    assert.equal(view.wallpaperPreviews.length, 0);
    finish(); await operation; await settle();
    assert.equal(wallpaperButtons(view).find((button) => button.props["aria-pressed"]).props["data-wallpaper-id"], 4);
    assert.equal(view.wallpaperPreviews.length, 1);
    assert.ok(wallpaperButtons(view).every((button) => !button.props.disabled));
  } finally { unmount(); }
});

test("a rejected wallpaper change preserves the previous selection and can be retried", async () => {
  let reject = true;
  const view = fixture("gallery", {}, undefined, {
    selection: 2, apply: async () => { if (reject) throw new Error("Windows denied the change"); },
  });
  const unmount = await view.mount();
  try {
    await wallpaperButtons(view)[8].props.onClick();
    assert.equal(wallpaperButtons(view).find((button) => button.props["aria-pressed"]).props["data-wallpaper-id"], 2);
    assert.equal(view.wallpaperPreviews.length, 0);
    assert.match(renderToStaticMarkup(view.body()), /role="alert"/);
    assert.ok(renderToStaticMarkup(view.body()).includes(texts.en.wallpaperApplyError));
    reject = false;
    await wallpaperButtons(view)[8].props.onClick();
    assert.doesNotMatch(renderToStaticMarkup(view.body()), /role="alert"/);
    assert.equal(wallpaperButtons(view).find((button) => button.props["aria-pressed"]).props["data-wallpaper-id"], 9);
    assert.deepEqual(view.wallpaperCalls, [{ id: 9 }, { id: 9 }]);
  } finally { unmount(); }
});

test("a late initial wallpaper read cannot overwrite an explicit successful choice", async () => {
  let read;
  const view = fixture("gallery", {}, undefined, { selection: () => new Promise((resolve) => { read = resolve; }) });
  const unmount = await view.mount();
  try {
    await wallpaperButtons(view)[11].props.onClick();
    read(1); await settle();
    assert.equal(wallpaperButtons(view).find((button) => button.props["aria-pressed"]).props["data-wallpaper-id"], 12);
    assert.deepEqual(view.wallpaperCalls, [{ id: 12 }]);
  } finally { unmount(); }
});

test("Bluetooth has no duplicate setting even with an old saved visibility override", () => {
  for (const language of Object.keys(texts)) for (const menuBarBluetooth of [false, true]) {
    const view = fixture("menubar", { language, menuBar: true, menuBarBluetooth });
    assert.doesNotMatch(renderToStaticMarkup(view.body()), /Bluetooth/i);
  }
});

test("retired Windows status-item overrides expose no controls and create no save", async () => {
  for (const language of Object.keys(texts)) for (const old of [null, false, true]) {
    const view = fixture("menubar", { language, menuBarSearch: old, menuBarTaskView: old,
      menuBarWidgets: old, menuBarBatteryPercent: old });
    const unmount = await view.mount();
    try {
      const controls = [...elements(view.body())].filter((node) => "checked" in node.props);
      assert.deepEqual(controls.map((node) => node.props.label), [texts[language].menuBarShow,
        texts[language].menuBarAllDisplays,
        texts[language].menuBarMusic, texts[language].aiUsage,
        texts[language].accessoryBattery, texts[language].menuBarSystemMonitor,
        texts[language].menuBarNotifications]);
      view.fire(60);
      await settle();
      assert.deepEqual(view.calls, []);
      assert.equal(await view.close(), false);
    } finally { unmount(); }
  }
});

test("the notifications button is opt-in and the clock target can be chosen and reset", async () => {
  const picked = "C:\\Apps\\Calendar.lnk";
  const native = { clockTarget: picked };
  const view = fixture("menubar", { menuBarNotifications: undefined, menuBarClockTarget: undefined }, async () => {}, native);
  const bell = () => [...elements(view.body())].find((node) =>
    node.props.label === texts.en.menuBarNotifications && "checked" in node.props).props.checked;
  const markup = () => renderToStaticMarkup(view.body());
  const unmount = await view.mount();
  try {
    assert.equal(bell(), false);
    assert.match(markup(), new RegExp(texts.en.clockTargetDefault));
    assert.doesNotMatch(markup(), new RegExp(texts.en.clockTargetReset));
    view.change(texts.en.menuBarNotifications, true);
    view.fire(60);
    await settle();
    assert.equal(bell(), true);
    assert.deepEqual(view.calls, [{ menuBarNotifications: true }]);

    view.button(texts.en.clockTargetChoose);
    await settle();
    view.fire(60);
    await settle();
    assert.deepEqual(view.clockPicks, [{ current: null, apps: texts.en.clockTargetApps, all: texts.en.clockTargetAllFiles }]);
    assert.deepEqual(view.calls.at(-1), { menuBarClockTarget: picked });
    assert.ok(markup().includes(picked));

    // Cancelling the dialog changes nothing.
    native.clockTarget = null;
    view.button(texts.en.clockTargetChoose);
    await settle();
    view.fire(60);
    await settle();
    assert.equal(view.calls.length, 2);
    assert.equal(view.clockPicks.at(-1).current, picked);

    view.button(texts.en.clockTargetReset);
    view.fire(60);
    await settle();
    assert.deepEqual(view.calls.at(-1), { menuBarClockTarget: null });
    assert.match(markup(), new RegExp(texts.en.clockTargetDefault));
  } finally { unmount(); }
});

test("other workspaces are opt-in and save without changing other window preferences", async () => {
  const view = fixture("windows", { showOtherWorkspaces: undefined });
  const checked = () => [...elements(view.body())].find((node) =>
    node.props.label === texts.en.showOtherWorkspaces && "checked" in node.props).props.checked;
  const unmount = await view.mount();
  try {
    assert.equal(checked(), false);
    view.change(texts.en.showOtherWorkspaces, true);
    view.fire(60);
    await settle();
    assert.equal(checked(), true);
    assert.deepEqual(view.calls, [{ showOtherWorkspaces: true }]);
    view.change(texts.en.showOtherWorkspaces, false);
    view.fire(60);
    await settle();
    assert.equal(checked(), false);
    assert.deepEqual(view.calls.at(-1), { showOtherWorkspaces: false });
  } finally { unmount(); }
});

test("one native dropdown preserves legacy modes and saves exclusive choices atomically", async () => {
  const modes = [
    [false, false, "visible"], [true, false, "automatic"],
    [false, true, "intelligent"], [true, true, "automatic"],
  ];
  for (const language of Object.keys(texts)) for (const [autoHide, smartHide, selected] of modes) {
    const view = fixture("dock", { language, autoHide, smartHide });
    const unmount = await view.mount();
    const selector = () => [...elements(view.body())].find((node) =>
      node.props.label === texts[language].dockHidingMode && "value" in node.props);
    try {
      assert.equal(selector().props.value, selected);
      assert.deepEqual(selector().props.options.map((option) => option.value), ["visible", "automatic", "intelligent"]);
      assert.deepEqual(view.calls, [], "opening Settings must not normalize saved preferences");
      const controls = [...elements(view.body())];
      assert.ok(!controls.some((node) => node.props.label === texts[language].dockHidingMode && "checked" in node.props));
      for (const mode of ["intelligent", "automatic", "visible"]) {
        view.change(texts[language].dockHidingMode, mode);
        view.fire(60);
        await settle();
        assert.equal(selector().props.value, mode);
        assert.deepEqual(view.calls.at(-1), { autoHide: mode === "automatic", smartHide: mode === "intelligent" });
        const html = renderToStaticMarkup(view.body());
        assert.ok(html.includes('<select class="select-control"'));
        const selectedOption = html.match(new RegExp(`<option[^>]*value="${mode}"[^>]*>`));
        assert.ok(selectedOption?.[0].includes('selected=""'));
        assert.ok(!html.includes('preview-hiding'));
      }
    } finally { unmount(); }
  }
});

test("reset selects the new intelligent default without changing pinned apps", async () => {
  const view = fixture("system", { autoHide: true, smartHide: false });
  const unmount = await view.mount();
  try {
    view.button(texts.en.resetButton);
    view.button(texts.en.resetConfirm);
    await settle();
    view.fire(60);
    await settle();
    assert.equal(view.calls.at(-1).autoHide, false);
    assert.equal(view.calls.at(-1).smartHide, true);
    assert.ok(!("pinned" in view.calls.at(-1)));
  } finally { unmount(); }
});

test("the mascot is Claude's crab unless the cat is chosen, and only while it is on", async () => {
  const picker = (view) => [...elements(view.body())].find((node) =>
    node.props.label === texts.en.mascotCharacter && "value" in node.props);
  const off = fixture("dock", { mascot: false, mascotCharacter: undefined });
  const unmountOff = await off.mount();
  try {
    // Folded away (inert) while the mascot is off.
    const folded = [...elements(off.body())].find((node) =>
      node.props.label === texts.en.mascotCharacter && "open" in node.props);
    assert.equal(folded.props.open, false);
  } finally { unmountOff(); }
  const view = fixture("dock", { mascot: true, mascotCharacter: undefined });
  const unmount = await view.mount();
  try {
    assert.equal(picker(view).props.value, "crab");
    assert.deepEqual(picker(view).props.options.map((option) => option.value), ["crab", "cat", "fox", "capybara", "mochi", "girl"]);
    // The break reminder is off unless chosen.
    const reminder = [...elements(view.body())].find((node) =>
      node.props.label === texts.en.mascotBreak && "checked" in node.props);
    assert.equal(reminder.props.checked, false);
    const companion = [...elements(view.body())].find((node) =>
      node.props.label === texts.en.mascotAlwaysVisible && "checked" in node.props);
    assert.equal(companion.props.checked, false);
    view.change(texts.en.mascotCharacter, "cat");
    view.change(texts.en.mascotAlwaysVisible, true);
    view.change(texts.en.mascotBreak, true);
    view.fire(60);
    await settle();
    assert.deepEqual(view.calls, [{ mascotCharacter: "cat", mascotAlwaysVisible: true, mascotBreakReminder: true }]);
  } finally { unmount(); }
});

test("mascot selections and reselects request one preview of the latest choice only", async () => {
  const view = fixture("dock", { mascot: true, mascotCharacter: "crab" });
  const unmount = await view.mount();
  try {
    assert.deepEqual(view.mascotPreviews, [], "opening Settings must not play a preview");
    view.change(texts.en.mascotCharacter, "cat");
    view.change(texts.en.mascotCharacter, "fox");
    view.fire(60); await settle();
    assert.deepEqual(view.calls, [{ mascotCharacter: "fox" }]);
    assert.deepEqual(view.mascotPreviews, [true]);
    const picker = [...elements(view.body())].find((node) =>
      node.props.label === texts.en.mascotCharacter && "onReselect" in node.props);
    picker.props.onReselect("fox");
    view.fire(60); await settle();
    assert.deepEqual(view.mascotPreviews, [true, true]);
    view.change(texts.en.mascotBreak, true);
    view.fire(60); await settle();
    assert.deepEqual(view.mascotPreviews, [true, true, false]);
  } finally { unmount(); }
});

test("a failed mascot save retries its preview with the newer selected character", async () => {
  let fail = true;
  const view = fixture("dock", { mascot: true, mascotCharacter: "crab" }, async () => {
    if (fail) throw new Error("fixture write failure");
  });
  const unmount = await view.mount();
  try {
    view.change(texts.en.mascotCharacter, "cat");
    view.fire(60); await settle();
    fail = false;
    view.change(texts.en.mascotCharacter, "girl");
    view.fire(60); await settle();
    assert.deepEqual(view.calls, [{ mascotCharacter: "cat" }, { mascotCharacter: "girl" }]);
    assert.deepEqual(view.mascotPreviews, [true, true]);
  } finally { unmount(); }
});

test("menu bar visibility is one choice of three, like the dock's", async () => {
  const view = fixture("menubar");
  const unmount = await view.mount();
  try {
    const select = [...elements(view.body())].find((node) =>
      node.props.label === texts.en.menuBarHidingMode && "value" in node.props);
    assert.equal(select.props.value, "visible");
    view.change(texts.en.menuBarHidingMode, "intelligent");
    view.change(texts.en.menuBarHidingMode, "automatic");
    view.fire(60);
    await settle();
    assert.deepEqual(view.calls, [{ menuBarAutoHide: true, menuBarSmartHide: false }]);
  } finally { unmount(); }
});

test("failed writes remain retryable and prevent closing until saved", async () => {
  let fail = true;
  const view = fixture("menubar", {}, async () => { if (fail) throw new Error("disk busy"); });
  const unmount = await view.mount();
  try {
    view.change(texts.en.menuBarMusic, false);
    view.fire(60);
    await settle();
    assert.equal(await view.close(), true);
    assert.equal(view.calls.length, 2);
    assert.ok(renderToStaticMarkup(view.body()).includes(texts.en.saveError));
    fail = false;
    assert.equal(await view.close(), false);
    assert.deepEqual(view.calls, Array(3).fill({ menuBarMusic: false }));
    assert.ok(!renderToStaticMarkup(view.body()).includes(texts.en.saveError));
  } finally { unmount(); }
});

test("a queued newer choice wins over an earlier failed write", async () => {
  let rejectFirst;
  const view = fixture("menubar", {}, () => view.calls.length === 1
    ? new Promise((_, reject) => { rejectFirst = reject; }) : Promise.resolve());
  const unmount = await view.mount();
  try {
    view.change(texts.en.menuBarMusic, false);
    view.fire(60);
    await settle();
    view.change(texts.en.menuBarMusic, true);
    view.change(texts.en.aiUsage, false);
    view.fire(60);
    rejectFirst(new Error("disk busy"));
    await settle();
    assert.deepEqual(view.calls, [{ menuBarMusic: false }, { menuBarMusic: true, aiUsage: false }]);
    assert.equal(await view.close(), false);
    assert.equal(view.calls.length, 2, "the obsolete value must not be replayed on close");
  } finally { unmount(); }
});

test("confirming reset cancels its old confirmation timer", async () => {
  const view = fixture("system");
  const unmount = await view.mount();
  try {
    view.button(texts.en.resetButton);
    assert.equal([...view.timers.values()].filter((timer) => timer.delay === 4000).length, 1);
    view.button(texts.en.resetConfirm);
    assert.equal([...view.timers.values()].filter((timer) => timer.delay === 4000).length, 0);
    await settle();
    view.button(texts.en.resetButton);
    assert.equal([...view.timers.values()].filter((timer) => timer.delay === 4000).length, 1);
  } finally { unmount(); }
});

test("dock choices save only the selected preferences and preserve icon size", async () => {
  const view = fixture("dock", { autoHide: true, iconSize: 64 });
  const unmount = await view.mount();
  try {
    view.change(texts.en.showStart, false);
    view.change(texts.en.pinExplorer, false);
    view.change(texts.en.showTrash, false);
    view.change(texts.en.showDownloads, false);
    view.change(texts.en.dockDensity, "compact");
    view.change(texts.en.dockAutoHideSpeed, "fast");
    view.fire(60);
    await settle();
    assert.deepEqual(view.calls, [{ showStart: false, pinExplorer: false, showTrash: false, showDownloads: false,
      dockDensity: "compact", dockAutoHideSpeed: "fast" }]);
    const controls = [...elements(view.body())];
    for (const label of [texts.en.showStart, texts.en.pinExplorer, texts.en.showTrash, texts.en.showDownloads]) {
      assert.equal(controls.find((node) => node.props.label === label && "checked" in node.props).props.checked, false);
    }
    assert.equal(controls.find((node) => node.props.label === texts.en.size && "value" in node.props).props.value, 64);
    const html = renderToStaticMarkup(view.body());
    assert.ok(html.includes('checked="" value="compact"'));
    assert.ok(html.includes('checked="" value="fast"'));
  } finally { unmount(); }
});

test("returning to Settings reflects dock removals without overwriting local edits", async () => {
  let finishSave;
  const view = fixture("dock", {}, () => new Promise((resolve) => { finishSave = resolve; }));
  const unmount = await view.mount();
  const checked = (label) => [...elements(view.body())]
    .find((node) => node.props.label === label && "checked" in node.props).props.checked;
  try {
    view.external({ showDownloads: false });
    assert.equal(checked(texts.en.showDownloads), true);
    await view.focus();
    assert.equal(checked(texts.en.showDownloads), false);
    view.change(texts.en.showDownloads, true);
    await view.focus(); // Flushes the unsaved change before requesting another snapshot.
    view.change(texts.en.showTrash, false);
    finishSave();
    await settle();
    assert.equal(checked(texts.en.showDownloads), true);
    assert.equal(checked(texts.en.showTrash), false);
    assert.deepEqual(view.calls, [{ showDownloads: true }]);
  } finally { unmount(); }
});

test("the theme picker offers every theme with a preview, saves only the choice and themes Settings", async () => {
  const themes = ["auto", "light", "dark", "frosted", "clear", "liquidGlass"];
  for (const language of Object.keys(texts)) {
    const view = fixture("appearance", { language });
    const picker = [...elements(view.body())].find((node) => node.props.label === texts[language].theme);
    assert.deepEqual(picker.props.options.map((option) => option.value), themes);
    assert.ok(picker.props.options.every((option) => option.preview && option.label));
    assert.equal(picker.props.value, "auto");
  }
  const view = fixture("appearance");
  const unmount = await view.mount();
  try {
    assert.deepEqual(view.themes, [null], "automatic follows Windows");
    view.change(texts.en.theme, "frosted");
    view.fire(60);
    await settle();
    assert.deepEqual(view.calls, [{ appearance: "frosted" }]);
    // An old file without the setting reads as automatic.
    const old = fixture("appearance", { appearance: undefined });
    const picker = [...elements(old.body())].find((node) => node.props.label === texts.en.theme);
    assert.equal(picker.props.value, "auto");
  } finally { unmount(); }
});

test("material presets follow the Windows app theme instead of forcing Settings light or dark", async () => {
  for (const appearance of ["auto", "frosted", "clear", "liquidGlass", "light", "dark"]) {
    const view = fixture("appearance", { appearance });
    const unmount = await view.mount();
    try {
      assert.deepEqual(view.themes, [appearance === "light" || appearance === "dark" ? appearance : null], appearance);
      assert.deepEqual(view.calls, [], "following Windows must not rewrite user preferences");
    } finally { unmount(); }
  }
});

test("the frosted preview explains its adaptive palette without adding another preference", () => {
  const view = fixture("appearance", { appearance: "frosted" });
  const picker = [...elements(view.body())].find((node) => node.props.label === texts.en.theme);
  const frosted = picker.props.options.find((option) => option.value === "frosted");
  assert.equal(frosted.detail, texts.en.themeAutoDetail);
  const html = renderToStaticMarkup(frosted.preview);
  assert.equal((html.match(/data-theme="frosted"/g) ?? []).length, 2);
  assert.match(html, /data-dark=""/);
  assert.match(html, /data-half=""/);
});

test("Liquid Glass replaces Aurora and previews both palettes for the dock and menu bar", async () => {
  const view = fixture("appearance");
  const picker = [...elements(view.body())].find((node) => node.props.label === texts.en.theme);
  assert.ok(!picker.props.options.some((option) => option.value === "aurora"));
  const liquid = picker.props.options.find((option) => option.value === "liquidGlass");
  assert.equal(liquid.detail, texts.en.themeAutoDetail);
  const html = renderToStaticMarkup(liquid.preview);
  assert.equal((html.match(/data-theme="liquidGlass"/g) ?? []).length, 2);
  assert.match(html, /data-dark=""/);
  const unmount = await view.mount();
  try {
    view.change(texts.en.theme, "liquidGlass");
    view.fire(60);
    await settle();
    assert.deepEqual(view.calls, [{ appearance: "liquidGlass" }]);
    assert.ok(view.themes.every((theme) => theme === null));
  } finally { unmount(); }
});

test("the menu bar shares the dock theme until given its own, which saves on its own key", async () => {
  for (const language of Object.keys(texts)) {
    const view = fixture("appearance", { language, appearance: "liquidGlass", menuBarAppearance: undefined });
    const nodes = [...elements(view.body())];
    const same = nodes.find((node) => node.props.label === texts[language].menuBarThemeSame && "checked" in node.props);
    assert.equal(same.props.checked, true);
    const second = nodes.find((node) => node.props.label === texts[language].menuBarTheme && node.props.options);
    assert.equal(second.props.value, "liquidGlass", "the closed picker shows the dock's theme");
    const row = nodes.find((node) => node.props.label === texts[language].menuBarTheme && "open" in node.props);
    assert.equal(row.props.open, false);
  }

  const view = fixture("appearance", { appearance: "liquidGlass", menuBarAppearance: null });
  const unmount = await view.mount();
  try {
    view.change(texts.en.menuBarThemeSame, false);
    view.fire(60);
    await settle();
    // Starts from the dock's theme: nothing changes until another is picked.
    assert.deepEqual(view.calls, [{ menuBarAppearance: "liquidGlass" }]);
    const nodes = [...elements(view.body())];
    assert.ok(nodes.find((node) => node.props.label === texts.en.themeDock && node.props.options));
    assert.equal(nodes.find((node) => node.props.label === texts.en.menuBarTheme && "open" in node.props).props.open, true);
    view.change(texts.en.menuBarTheme, "dark");
    view.fire(60);
    await settle();
    assert.deepEqual(view.calls.at(-1), { menuBarAppearance: "dark" });
    // The dock's picker still saves only the dock's theme.
    view.change(texts.en.themeDock, "light");
    view.fire(60);
    await settle();
    assert.deepEqual(view.calls.at(-1), { appearance: "light" });
    view.change(texts.en.menuBarThemeSame, true);
    view.fire(60);
    await settle();
    assert.deepEqual(view.calls.at(-1), { menuBarAppearance: null });
  } finally { unmount(); }
});

test("the alert bounce and the preview close button are always on, with no switch left", () => {
  for (const section of ["dock", "windows"]) for (const old of [false, true]) {
    const html = renderToStaticMarkup(fixture(section, { notificationBounce: old, showPreviewCloseButton: old }).body());
    assert.doesNotMatch(html, /Bounce icon on alert|close button on previews/i);
  }
});


test("all translations have complete keys, wallpaper names and intact placeholders", () => {
  const placeholders = (text) => [...text.matchAll(/\{[^}]+\}/g)].map(([match]) => match).sort();
  for (const [language, catalog] of Object.entries(texts)) {
    assert.deepEqual(Object.keys(catalog).sort(), Object.keys(texts.en).sort(), language);
    for (const [key, original] of Object.entries(texts.en)) {
      const values = Array.isArray(original) ? catalog[key] : [catalog[key]];
      const originals = Array.isArray(original) ? original : [original];
      assert.equal(values.length, originals.length, `${language}/${key}`);
      values.forEach((value, index) => {
        assert.equal(typeof value, "string");
        assert.ok(value.trim(), `${language}/${key}`);
        assert.deepEqual(placeholders(value), placeholders(originals[index]), `${language}/${key}`);
      });
    }
  }
});

test("language uses one dropdown and changing it preserves other preferences", async () => {
  for (const language of Object.keys(texts)) {
    const view = fixture("system", { language });
    const control = [...elements(view.body())].find((node) => node.props.label === texts[language].language && typeof node.props.onChange === "function");
    const html = renderToStaticMarkup(control);
    assert.match(html, /<select /);
    assert.doesNotMatch(html, /radiogroup/);
    assert.equal((html.match(/<option /g) ?? []).length, 9);
    for (const other of Object.keys(texts)) {
      view.change(texts[language].language, other);
      view.fire(60); await settle();
      assert.deepEqual(view.calls.at(-1), { language: other });
      const root = view.render();
      assert.equal(root.props.lang, other);
      assert.equal(root.props.dir, other === "ar" ? "rtl" : "ltr");
      // Return through the current locale's control, not a stale label.
      view.change(texts[other].language, language);
      view.fire(60); await settle();
    }
  }
});


test("weather is opt-in and ignores a late location after disabling", async () => {
  let resolve;
  const view = fixture("weather", {}, undefined, { locateWeather: () => new Promise((done) => { resolve = done; }) });
  const unmount = await view.mount();
  try {
    assert.deepEqual(view.weatherCalls, []);
    assert.equal(view.timers.size, 0);
    view.change(texts.en.weatherShow, true);
    assert.deepEqual(view.weatherCalls, ["locate"]);
    view.change(texts.en.weatherShow, false);
    resolve({ latitudeE2: 3872, longitudeE2: -914, updatedAt: 123 });
    await settle();
    assert.deepEqual(view.calls, [{ weather: true }, { weather: false, weatherLocation: null, weatherAutoCity: null }]);
    assert.deepEqual(view.weatherCalls, ["locate", "cancel"]);
  } finally { unmount(); }
});

test("quick weather off/on serializes location prompts and keeps only the latest fix", async () => {
  const resolvers = [];
  const view = fixture("weather", {}, undefined, { locateWeather: () => new Promise((done) => resolvers.push(done)) });
  const unmount = await view.mount();
  try {
    view.change(texts.en.weatherShow, true);
    view.change(texts.en.weatherShow, false);
    view.change(texts.en.weatherShow, true);
    assert.equal(resolvers.length, 1);
    resolvers[0]({ latitudeE2: 100, longitudeE2: 100, updatedAt: 123 });
    await settle();
    assert.equal(resolvers.length, 2);
    const latest = { latitudeE2: 3872, longitudeE2: -914, updatedAt: Math.floor(Date.now()/1000) };
    resolvers[1](latest);
    await settle();
    assert.deepEqual(view.calls.at(-1), { weatherLocation: latest, weatherAutoCity: null });
    assert.ok(!view.calls.some((patch) => patch.weatherLocation?.latitudeE2 === 100));
  } finally { unmount(); }
});

test("weather permission denial has recovery controls and never invents a location", async () => {
  const view = fixture("weather", {}, undefined, { locateWeather: async () => { throw "denied"; } });
  const unmount = await view.mount();
  try {
    view.change(texts.en.weatherShow, true);
    await settle();
    assert.deepEqual(view.calls.at(-1), { weatherLocation: null, weatherAutoCity: null });
    const html = renderToStaticMarkup(view.body());
    assert.ok(html.includes(texts.en.weatherDenied));
    view.button(texts.en.weatherLocationPrivacy);
    await settle();
    assert.deepEqual(view.weatherCalls, ["locate", "privacy"]);
    assert.equal(view.timers.size, 0);
  } finally { unmount(); }
});

test("fresh weather locations are reused and old fixes refresh only while Settings is open", async () => {
  const now = Math.floor(Date.now()/1000);
  for (const age of [0, 7*3600]) {
    const location = { latitudeE2: 3872, longitudeE2: -914, updatedAt: now-age };
    const view = fixture("weather", { weather: true, weatherLocation: location,
      weatherAutoCity: { label: "Lisboa", latitudeE2: 3872, longitudeE2: -914 } }, undefined,
      { locateWeather: async () => ({ ...location, updatedAt: now }) });
    const unmount = await view.mount();
    try {
      assert.deepEqual(view.weatherCalls, age ? ["locate"] : []);
      assert.equal(view.timers.size, 0);
    } finally { unmount(); }
  }
});


test("manual weather searches only on submit, saves a city and reuses the automatic fix", async () => {
  const city = { label: "Lima, Perú", latitudeE2: -1205, longitudeE2: -7704 };
  const fix = { latitudeE2: 3872, longitudeE2: -914, updatedAt: Math.floor(Date.now()/1000) };
  let finish;
  const view = fixture("weather", { weather: true, weatherLocationMode: "city", weatherLocation: fix }, undefined,
    { searchCities: () => new Promise((done) => { finish = done; }) });
  const unmount = await view.mount();
  try {
    assert.deepEqual(view.weatherCalls, []);
    view.input(texts.en.weatherCity, "Lima");
    assert.deepEqual(view.weatherCalls, [], "typing must never perform HTTP");
    view.submit(); view.submit();
    assert.deepEqual(view.weatherCalls, [{ search: "Lima", language: "en" }]);
    finish([city]); await settle();
    view.button(city.label);
    assert.deepEqual(view.calls.at(-1), { weatherCity: city });
    view.change(texts.en.weatherLocation, "auto");
    assert.deepEqual(view.calls.at(-1), { weatherLocationMode: "auto" });
    assert.equal(view.weatherCalls.length, 1, "fresh automatic fix needs no new permission request");
    assert.equal(view.timers.size, 0);
  } finally { unmount(); }
});

test("changing weather mode ignores late Windows fixes and city search responses", async () => {
  let locate, search;
  const view = fixture("weather", {}, undefined, {
    locateWeather: () => new Promise((done) => { locate = done; }),
    searchCities: () => new Promise((done) => { search = done; }),
  });
  const unmount = await view.mount();
  try {
    view.change(texts.en.weatherShow, true);
    view.change(texts.en.weatherLocation, "city");
    locate({ latitudeE2: 100, longitudeE2: 100, updatedAt: 123 });
    await settle();
    assert.ok(!view.calls.some((patch) => patch.weatherLocation));
    view.input(texts.en.weatherCity, "Paris"); view.submit();
    view.change(texts.en.weatherShow, false);
    search([{ label: "Paris, France", latitudeE2: 4886, longitudeE2: 235 }]);
    await settle();
    assert.ok(!renderToStaticMarkup(view.body()).includes("Paris, France"));
    assert.ok(!view.calls.some((patch) => patch.weatherCity));
    assert.equal(view.timers.size, 0);
  } finally { unmount(); }
});

test("city search shows empty results, ignores edited queries and stays off on mount", async () => {
  let resolve;
  const view = fixture("weather", { weather: true, weatherLocationMode: "city" }, undefined,
    { searchCities: () => new Promise((done) => { resolve = done; }) });
  const unmount = await view.mount();
  try {
    view.input(texts.en.weatherCity, "A"); view.submit();
    assert.deepEqual(view.weatherCalls, []);
    view.input(texts.en.weatherCity, "Paris"); view.submit();
    view.input(texts.en.weatherCity, "Lima");
    resolve([{ label: "Paris, France", latitudeE2: 4886, longitudeE2: 235 }]); await settle();
    assert.ok(!renderToStaticMarkup(view.body()).includes("Paris, France"));
    view.submit(); resolve([]); await settle();
    assert.ok(renderToStaticMarkup(view.body()).includes(texts.en.weatherNoCities));
  } finally { unmount(); }
});


test("city search can retry after a network failure without changing the saved city", async () => {
  const saved = { label: "Lima, Perú", latitudeE2: -1205, longitudeE2: -7704 };
  let attempts = 0;
  const view = fixture("weather", { weather: true, weatherLocationMode: "city", weatherCity: saved }, undefined,
    { searchCities: async () => { if (++attempts === 1) throw "unavailable"; return []; } });
  const unmount = await view.mount();
  try {
    view.input(texts.en.weatherCity, "Paris"); view.submit(); await settle();
    assert.ok(renderToStaticMarkup(view.body()).includes(texts.en.weatherSearchError));
    view.submit(); await settle();
    assert.equal(attempts, 2);
    assert.ok(renderToStaticMarkup(view.body()).includes(texts.en.weatherNoCities));
    assert.deepEqual(view.calls, []);
  } finally { unmount(); }
});


test("automatic weather saves and displays the resolved city without the removed helper/footer", async () => {
  const city = { label: "Lima", latitudeE2: -1205, longitudeE2: -7704 };
  const fix = { latitudeE2: city.latitudeE2, longitudeE2: city.longitudeE2, updatedAt: Math.floor(Date.now()/1000) };
  const view = fixture("weather", {}, undefined, { locateWeather: async () => ({ ...fix, city }) });
  const unmount = await view.mount();
  try {
    view.change(texts.en.weatherShow, true); await settle();
    assert.deepEqual(view.calls.at(-1), { weatherLocation: fix, weatherAutoCity: city });
    const html = renderToStaticMarkup(view.body());
    assert.ok(html.includes("Lima"));
    assert.ok(!html.includes(texts.en.weatherLocationSaved));
    assert.ok(!html.includes(texts.en.weatherAttribution));
    assert.equal(view.timers.size, 0);
  } finally { unmount(); }
});

test("old automatic fixes without city names are resolved once and failures keep valid weather", async () => {
  const fix = { latitudeE2: 3872, longitudeE2: -914, updatedAt: Math.floor(Date.now()/1000) };
  const view = fixture("weather", { weather: true, weatherLocation: fix }, undefined,
    { locateWeather: async () => ({ ...fix, city: null }) });
  const unmount = await view.mount();
  try {
    assert.deepEqual(view.weatherCalls, ["locate"]);
    assert.deepEqual(view.calls.at(-1), { weatherLocation: fix, weatherAutoCity: null });
    view.render();
    assert.equal(view.weatherCalls.length, 1);
    assert.equal(view.timers.size, 0);
  } finally { unmount(); }
});
