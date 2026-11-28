import { useEffect, useRef, useState } from "react";
import {
  type SearchConfig,
  type SearchEngine,
  type SearchKeyword,
  type SearchMonitor,
  type SearchStatus,
  SEARCH_LIMITS,
  clearSearchHistory,
  loadSearchStatus,
  pickFolder,
  rebuildSearchIndex,
  suspendSearchHotkey,
} from "./config";
import { Card, Row, Select, Slider, SubRow, Toggle } from "./controls";
import type { Texts } from "./i18n";

/** The search process applies a change a moment after it is saved. */
const STATUS_DELAY_MS = 700;
const REFRESH_MINUTES = [1, 2, 5, 15, 60] as const;
/** Product names, the same in every language. */
const ENGINES: readonly { value: SearchEngine; label: string }[] = [
  { value: "google", label: "Google" },
  { value: "bing", label: "Bing" },
  { value: "duckduckgo", label: "DuckDuckGo" },
  { value: "brave", label: "Brave Search" },
  { value: "ecosia", label: "Ecosia" },
  { value: "startpage", label: "Startpage" },
];

interface KeyPress {
  keyCode: number;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

/**
 * A key press written as `muelle_config::Hotkey` reads it. `undefined` while
 * only modifiers are down; `null` for a combination that cannot be claimed.
 */
export function shortcutFrom(press: KeyPress): string | null | undefined {
  const { keyCode } = press;
  if ([16, 17, 18, 91, 92].includes(keyCode)) return undefined;
  const key = keyCode === 32 ? "Space"
    : (keyCode >= 48 && keyCode <= 57) || (keyCode >= 65 && keyCode <= 90) ? String.fromCharCode(keyCode)
    : keyCode >= 112 && keyCode <= 123 ? `F${keyCode - 111}`
    : null;
  // Without Ctrl, Alt or Win a global shortcut would swallow typing.
  if (!key || !(press.ctrlKey || press.altKey || press.metaKey)) return null;
  const shortcut = [
    press.metaKey ? "Win" : null,
    press.ctrlKey ? "Ctrl" : null,
    press.altKey ? "Alt" : null,
    press.shiftKey ? "Shift" : null,
    key,
  ].filter(Boolean).join("+");
  // Alt+F4 must keep closing windows.
  return shortcut === "Alt+F4" ? null : shortcut;
}

/** As `muelle_config::is_search_address`: a web address with room for the words. */
export const isSearchAddress = (url: string) =>
  /^https?:\/\/\S+$/.test(url) && url.includes("{query}");

/** As `muelle_config::SearchKeyword::new`. */
export function keywordFrom(word: string, url: string): SearchKeyword | null {
  const entry = { word: word.trim().toLowerCase(), url: url.trim() };
  return entry.word && !/[\s:]/.test(entry.word) && isSearchAddress(entry.url) ? entry : null;
}

interface SearchSectionProps {
  t: Texts;
  search: SearchConfig;
  onChange: (patch: Partial<SearchConfig>) => void;
  /** Writes pending changes now; resolves to `false` if that failed. */
  flush: () => Promise<boolean>;
}

export function SearchSection({ t, search, onChange, flush }: SearchSectionProps) {
  const [status, setStatus] = useState<SearchStatus | null>(null);
  const [recording, setRecording] = useState(false);
  const [rejected, setRejected] = useState(false);
  const [failed, setFailed] = useState(false);
  const [cleared, setCleared] = useState(false);
  const held = useRef(false);
  /** The key release that follows a recorded press must not press the button. */
  const swallowRelease = useRef(false);

  useEffect(() => {
    let alive = true;
    const read = () => {
      loadSearchStatus()
        .then((found) => { if (alive) setStatus(found); })
        .catch(() => { });
    };
    const timer = window.setTimeout(read, STATUS_DELAY_MS);
    window.addEventListener("focus", read);
    return () => {
      alive = false;
      window.clearTimeout(timer);
      window.removeEventListener("focus", read);
    };
  }, [search.enabled, search.hotkey]);

  // Leaving the page while recording gives the shortcut back.
  useEffect(() => () => {
    if (held.current) void suspendSearchHotkey(false).catch(() => { });
  }, []);

  const run = (action: () => Promise<unknown>) => {
    setFailed(false);
    action().catch(() => setFailed(true));
  };

  const record = () => {
    held.current = true;
    setRejected(false);
    setRecording(true);
    run(() => suspendSearchHotkey(true));
  };

  const stopRecording = (shortcut?: string) => {
    if (!held.current) return;
    held.current = false;
    setRecording(false);
    if (shortcut) onChange({ hotkey: shortcut });
    // The search process reads the new shortcut from the saved file.
    run(async () => {
      await flush();
      await suspendSearchHotkey(false);
      if (shortcut) setStatus(await loadSearchStatus());
    });
  };

  const addFolder = (list: "folders" | "excludedFolders") => {
    run(async () => {
      const folder = await pickFolder(null);
      if (folder && !search[list].includes(folder)) onChange({ [list]: [...search[list], folder] });
    });
  };

  const shortcutHint = recording ? (rejected ? t.searchShortcutInvalid : t.searchShortcutRecording)
    : !status ? t.searchShortcutHint
    : !status.running ? t.searchNotRunning
    : !status.hotkey ? t.searchShortcutNone
    : status.hotkey !== search.hotkey ? t.searchShortcutTaken.replace("{shortcut}", status.hotkey)
    : t.searchShortcutHint;

  const toggle = (
    key: "keepQuery" | "closeOnBlur" | "compact" | "headers" | "showApps" | "showSystemSettings" |
      "showFolders" | "showFiles" | "showContents" | "showDefinitions" | "showCalculator" |
      "showAddresses" | "showWebSearch" | "learn" | "hiddenFiles" | "keywordsEnabled",
    label: string,
    hint?: string,
  ) => (
    <Row label={label} hint={hint}>
      <Toggle label={label} checked={search[key]} onChange={(on) => onChange({ [key]: on })} />
    </Row>
  );

  const folderList = (list: "folders" | "excludedFolders", label: string) => (
    <>
      <Row label={label}>
        <button type="button" className="push" onClick={() => addFolder(list)}>{t.searchAddFolder}</button>
      </Row>
      {search[list].map((folder) => (
        <div className="list-item" key={folder}>
          <span dir="ltr">{folder}</span>
          <button type="button" className="push" aria-label={`${t.searchRemove}: ${folder}`}
            onClick={() => onChange({ [list]: search[list].filter((other) => other !== folder) })}>
            {t.searchRemove}
          </button>
        </div>
      ))}
    </>
  );

  const standard = [
    ["desktop", t.searchDesktop],
    ["documents", t.searchDocuments],
    ["downloads", t.searchDownloads],
    ["pictures", t.searchPictures],
    ["music", t.searchMusic],
    ["videos", t.searchVideos],
  ] as const;

  return (
    <>
      <Card>
        <Row label={t.searchEnabled} hint={t.searchEnabledHint}>
          <Toggle label={t.searchEnabled} checked={search.enabled} onChange={(enabled) => onChange({ enabled })} />
        </Row>
        <SubRow label={t.searchShortcut} hint={shortcutHint} open={search.enabled}>
          <button
            type="button"
            className={recording ? "push push-primary" : "push"}
            aria-label={`${t.searchShortcut}: ${search.hotkey}`}
            onClick={() => { if (!recording) record(); }}
            onBlur={() => stopRecording()}
            onKeyDown={(event) => {
              // Tab still moves on, which ends the recording.
              if (!recording || event.key === "Tab" && !event.altKey && !event.ctrlKey) return;
              event.preventDefault();
              swallowRelease.current = true;
              if (event.key === "Escape") return stopRecording();
              const shortcut = shortcutFrom(event);
              if (shortcut === null) setRejected(true);
              else if (shortcut) stopRecording(shortcut);
            }}
            // Releasing Space would otherwise press the button again.
            onKeyUp={(event) => {
              if (swallowRelease.current) event.preventDefault();
              swallowRelease.current = false;
            }}
          >
            <span dir="ltr">{recording ? "…" : search.hotkey}</span>
          </button>
        </SubRow>
        <SubRow label={t.searchMonitor} open={search.enabled}>
          <Select<SearchMonitor>
            label={t.searchMonitor}
            value={search.monitor}
            options={[
              { value: "mouse", label: t.searchMonitorMouse },
              { value: "active", label: t.searchMonitorActive },
              { value: "primary", label: t.searchMonitorPrimary },
            ]}
            onChange={(monitor) => onChange({ monitor })}
          />
        </SubRow>
      </Card>
      {failed ? <p className="error" role="alert">{t.searchActionError}</p> : null}
      {search.enabled ? (
        <>
          <Card title={t.searchGroupWindow}>
            {toggle("keepQuery", t.searchKeepQuery)}
            {toggle("closeOnBlur", t.searchCloseOnBlur)}
            <Row label={t.searchWidth}>
              <Slider label={t.searchWidth} value={search.width} {...SEARCH_LIMITS.width}
                onChange={(width) => onChange({ width })} />
            </Row>
            <Row label={t.searchPosition}>
              <Slider label={t.searchPosition} value={search.position} {...SEARCH_LIMITS.position} unit="%"
                onChange={(position) => onChange({ position })} />
            </Row>
            <Row label={t.searchRows}>
              <Slider label={t.searchRows} value={search.maxRows} {...SEARCH_LIMITS.maxRows} unit=""
                onChange={(maxRows) => onChange({ maxRows })} />
            </Row>
            {toggle("compact", t.searchCompact)}
            {toggle("headers", t.searchHeaders)}
            <p className="card-note row-hint">{t.searchWindowHint}</p>
          </Card>
          <Card title={t.searchGroupResults}>
            {toggle("showApps", t.searchShowApps)}
            {toggle("showSystemSettings", t.searchShowSystemSettings)}
            {toggle("showFolders", t.searchShowFolders)}
            {toggle("showFiles", t.searchShowFiles)}
            {toggle("showContents", t.searchShowContents, t.searchShowContentsHint)}
            <SubRow label={t.searchContentResults} open={search.showContents}>
              <Slider label={t.searchContentResults} value={search.contentResults}
                {...SEARCH_LIMITS.contentResults} unit=""
                onChange={(contentResults) => onChange({ contentResults })} />
            </SubRow>
            {toggle("showDefinitions", t.searchShowDefinitions,
              status?.dictionary === false ? t.searchDictionaryMissing : undefined)}
            {toggle("showCalculator", t.searchShowCalculator)}
            {toggle("showAddresses", t.searchShowAddresses)}
            {toggle("showWebSearch", t.searchShowWeb)}
            {toggle("learn", t.searchLearn)}
            <Row label={t.searchHistory} hint={cleared ? t.searchHistoryCleared : undefined}>
              <button type="button" className="push"
                onClick={() => run(() => clearSearchHistory().then(() => setCleared(true)))}>
                {t.searchHistoryClear}
              </button>
            </Row>
          </Card>
          <Card title={t.searchGroupFiles}>
            {standard.map(([key, label]) => (
              <Row label={label} key={key}>
                <Toggle label={label} checked={search.standardFolders[key]}
                  onChange={(on) => onChange({ standardFolders: { ...search.standardFolders, [key]: on } })} />
              </Row>
            ))}
            {folderList("folders", t.searchFolders)}
            {folderList("excludedFolders", t.searchExcluded)}
            {toggle("hiddenFiles", t.searchHiddenFiles)}
            <Row label={t.searchRefresh}>
              <Select
                label={t.searchRefresh}
                value={String(search.refreshMinutes)}
                // A value written by hand stays listed.
                options={[...new Set([...REFRESH_MINUTES, search.refreshMinutes])]
                  .sort((a, b) => a - b)
                  .map((minutes) => ({ value: String(minutes), label: t.searchMinutes.replace("{minutes}", String(minutes)) }))}
                onChange={(minutes) => onChange({ refreshMinutes: Number(minutes) })}
              />
            </Row>
            <Row label={t.searchRebuild}>
              <button type="button" className="push" onClick={() => run(rebuildSearchIndex)}>
                {t.searchRebuildAction}
              </button>
            </Row>
          </Card>
          <Card title={t.searchGroupWeb}>
            <Row label={t.searchEngine}>
              <Select<SearchEngine>
                label={t.searchEngine}
                value={search.engine}
                options={[...ENGINES, { value: "custom", label: t.searchEngineCustom }]}
                onChange={(engine) => onChange({ engine })}
              />
            </Row>
            {search.engine === "custom" ? (
              <AddressField t={t} saved={search.customSearch}
                onSave={(customSearch) => onChange({ customSearch })} />
            ) : null}
            {toggle("keywordsEnabled", t.searchKeywords, t.searchKeywordsHint)}
            {search.keywordsEnabled ? (
              <KeywordList t={t} keywords={search.keywords}
                onChange={(keywords) => onChange({ keywords })} />
            ) : null}
          </Card>
        </>
      ) : null}
    </>
  );
}

/** Saves only a usable address, so one being typed is not thrown away. */
function AddressField({ t, saved, onSave }: { t: Texts; saved: string; onSave: (url: string) => void }) {
  const [draft, setDraft] = useState(saved);
  const valid = isSearchAddress(draft.trim());
  return (
    <Row label={t.searchCustomAddress} hint={t.searchCustomAddressHint} stacked>
      <input className="text-input" type="url" dir="ltr" spellCheck={false}
        aria-label={t.searchCustomAddress} aria-invalid={draft !== "" && !valid}
        placeholder="https://example.com/?q={query}"
        value={draft}
        onChange={(event) => {
          setDraft(event.target.value);
          const url = event.target.value.trim();
          if (isSearchAddress(url)) onSave(url);
        }}
      />
    </Row>
  );
}

function KeywordList({ t, keywords, onChange }: {
  t: Texts;
  keywords: SearchKeyword[];
  onChange: (keywords: SearchKeyword[]) => void;
}) {
  const [word, setWord] = useState("");
  const [url, setUrl] = useState("");
  const entry = keywordFrom(word, url);
  return (
    <>
      {keywords.map((keyword) => (
        <div className="list-item" key={keyword.word}>
          <span dir="ltr"><strong>{keyword.word}</strong> {keyword.url}</span>
          <button type="button" className="push" aria-label={`${t.searchRemove}: ${keyword.word}`}
            onClick={() => onChange(keywords.filter((other) => other.word !== keyword.word))}>
            {t.searchRemove}
          </button>
        </div>
      ))}
      <form className="list-item list-form" onSubmit={(event) => {
        event.preventDefault();
        if (!entry) return;
        // An entry for the same word is replaced.
        onChange([...keywords.filter((other) => other.word !== entry.word), entry]);
        setWord("");
        setUrl("");
      }}>
        <input className="text-input text-input-short" dir="ltr" spellCheck={false}
          aria-label={t.searchKeywordWord} placeholder={t.searchKeywordWord}
          value={word} onChange={(event) => setWord(event.target.value)} />
        <input className="text-input" type="url" dir="ltr" spellCheck={false}
          aria-label={t.searchKeywordAddress} placeholder={t.searchKeywordAddress}
          value={url} onChange={(event) => setUrl(event.target.value)} />
        <button type="submit" className="push" disabled={!entry}>{t.searchKeywordAdd}</button>
      </form>
    </>
  );
}
