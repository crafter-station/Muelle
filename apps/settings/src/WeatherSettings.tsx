import { useCallback, useEffect, useRef, useState } from "react";
import { type Config, type WeatherLocation, type WeatherCity, type WeatherLocationMode, locateWeather, searchWeatherCities, cancelWeatherLocation, openWeatherLocationSettings, openWeatherAttribution } from "./config";
import { Card, Row, SubRow, Toggle, Select } from "./controls";
import type { Texts } from "./i18n";

/** Location consent belongs to this foreground process. No mount-time prompt
 * unless weather was already opted into and its stored fix needs renewing. */
export function WeatherSettings({ config, texts: t, onChange }: {
  config: Config; texts: Texts; onChange: (patch: Partial<Config>) => void;
}) {
  const [status, setStatus] = useState("");
  const mode = config.weatherLocationMode ?? "auto";
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<WeatherCity[]>([]);
  const [searchStatus, setSearchStatus] = useState("");
  const [searching, setSearching] = useState(false);
  const searchGeneration = useRef(0);
  const searchPending = useRef(false);
  const searchEnabled = useRef(!!config.weather && mode === "city");
  searchEnabled.current = !!config.weather && mode === "city";
  const desired = useRef(!!config.weather && mode === "auto");
  const generation = useRef(0);
  const mounted = useRef(true);
  const pending = useRef<Promise<WeatherLocation> | null>(null);
  const change = useRef(onChange);
  change.current = onChange;

  const locate = useCallback(function locate() {
    if (!desired.current || pending.current || !mounted.current) return;
    const expected = generation.current;
    setStatus("locating");
    const request = locateWeather();
    pending.current = request;
    request.then((location) => {
      if (mounted.current && desired.current && generation.current === expected) {
        change.current({ weatherLocation: location });
        setStatus("");
      }
    }).catch((error: unknown) => {
      if (mounted.current && desired.current && generation.current === expected) {
        setStatus(typeof error === "string" ? error : "unavailable");
        if (error === "denied") change.current({ weatherLocation: null });
      }
    }).finally(() => {
      pending.current = null;
      // A quick off/on waits for the cancelled native request, rather than
      // stacking permission prompts or keeping a retry timer alive.
      if (mounted.current && desired.current && generation.current !== expected) locate();
    });
  }, []);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      generation.current += 1;
      searchGeneration.current += 1;
      if (pending.current) void cancelWeatherLocation().catch(() => {});
    };
  }, []);

  useEffect(() => {
    const wasEnabled = desired.current;
    desired.current = !!config.weather && mode === "auto";
    if (!desired.current) {
      if (wasEnabled) generation.current += 1;
      if (pending.current) void cancelWeatherLocation().catch(() => {});
      return;
    }
    if (status && status !== "locating") return;
    if (!config.weatherLocation || Date.now() / 1000 - config.weatherLocation.updatedAt > 6 * 3600) locate();
  }, [config.weather, config.weatherLocation, mode, locate, status]);

  const toggle = (enabled: boolean) => {
    generation.current += 1;
    desired.current = enabled && mode === "auto";
    searchGeneration.current += 1;
    setResults([]);
    setSearchStatus("");
    setStatus("");
    onChange(enabled ? { weather: true } : { weather: false, weatherLocation: null });
    if (desired.current) locate();
    else if (pending.current) void cancelWeatherLocation().catch(() => {});
  };
  const chooseMode = (next: WeatherLocationMode) => {
    generation.current += 1;
    searchGeneration.current += 1;
    desired.current = !!config.weather && next === "auto";
    setStatus(""); setResults([]); setSearchStatus("");
    onChange({ weatherLocationMode: next });
    if (pending.current) void cancelWeatherLocation().catch(() => {});
    if (desired.current && (!config.weatherLocation || Date.now() / 1000 - config.weatherLocation.updatedAt > 6 * 3600)) locate();
  };
  const search = () => {
    const name = query.trim();
    if (searchPending.current || !config.weather || mode !== "city" || name.length < 2) return;
    const expected = ++searchGeneration.current;
    searchPending.current = true;
    setSearching(true); setResults([]); setSearchStatus("");
    const language = config.language === "auto" ? navigator.language.split("-")[0] : config.language;
    void searchWeatherCities(name, language).then((cities) => {
      if (!mounted.current || !searchEnabled.current || searchGeneration.current !== expected) return;
      setResults(cities);
      setSearchStatus(cities.length ? "" : "empty");
    }).catch(() => {
      if (mounted.current && searchEnabled.current && searchGeneration.current === expected) setSearchStatus("error");
    }).finally(() => {
      searchPending.current = false;
      if (mounted.current) setSearching(false);
    });
  };
  const message = status === "locating" ? t.weatherLocating
    : status === "denied" ? t.weatherDenied
    : status === "focus" ? t.weatherNeedsFocus
    : status ? t.weatherUnavailable
    : config.weatherLocation ? t.weatherLocationSaved : t.weatherNeedsLocation;

  return <Card title={t.weatherTitle}>
    <Row label={t.weatherShow} hint={t.weatherHint}>
      <Toggle label={t.weatherShow} checked={!!config.weather} onChange={toggle} />
    </Row>
    <SubRow label={t.weatherLocation} open={!!config.weather}>
      <Select label={t.weatherLocation} value={mode} onChange={chooseMode} options={[
        { value: "auto", label: t.weatherAutomatic }, { value: "city", label: t.weatherChooseCity },
      ]} />
    </SubRow>
    <SubRow label={t.weatherAutomatic} hint={message} open={!!config.weather && mode === "auto"} stacked>
      <div className="weather-settings-actions" aria-live="polite">
        <span className="sr-only" role="status">{message}</span>
        <button type="button" className="push" disabled={status === "locating"}
          onClick={() => { generation.current += 1; locate(); }}>{t.weatherUpdateLocation}</button>
        {status && status !== "locating" ? <button type="button" className="push"
          onClick={() => void openWeatherLocationSettings().catch(() => setStatus("unavailable"))}>{t.weatherLocationPrivacy}</button> : null}
      </div>
    </SubRow>
    <SubRow label={t.weatherCity} hint={config.weatherCity?.label ?? t.weatherCityHint}
      open={!!config.weather && mode === "city"} stacked>
      <form className="weather-city-search" onSubmit={(event) => { event.preventDefault(); search(); }}>
        <input type="search" aria-label={t.weatherCity} placeholder={t.weatherCityPlaceholder}
          maxLength={100} value={query} onChange={(event) => {
            searchGeneration.current += 1;
            setQuery(event.target.value); setResults([]); setSearchStatus("");
          }} />
        <button type="submit" className="push" disabled={searching || query.trim().length < 2}>
          {searching ? t.weatherSearching : t.weatherSearch}
        </button>
      </form>
      <div role="status" className="row-hint">
        {searchStatus === "empty" ? t.weatherNoCities : searchStatus === "error" ? t.weatherSearchError : ""}
      </div>
      {results.length ? <ul className="weather-city-results" aria-label={t.weatherCityResults}>
        {results.map((city) => <li key={`${city.label}:${city.latitudeE2}:${city.longitudeE2}`}>
          <button type="button" aria-pressed={config.weatherCity?.label === city.label
            && config.weatherCity?.latitudeE2 === city.latitudeE2 && config.weatherCity?.longitudeE2 === city.longitudeE2}
            onClick={() => { onChange({ weatherCity: city }); setResults([]); setQuery(""); }}>{city.label}</button>
        </li>)}
      </ul> : null}
    </SubRow>
    {config.weather ? <div className="weather-settings-credit">
      <button type="button" className="weather-credit" onClick={() => void openWeatherAttribution().catch(() => {})}>
        {t.weatherAttribution}
      </button>
    </div> : null}
  </Card>;
}
