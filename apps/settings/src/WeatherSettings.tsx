import { useCallback, useEffect, useRef, useState } from "react";
import { type Config, type WeatherLocation, locateWeather, cancelWeatherLocation, openWeatherLocationSettings, openWeatherAttribution } from "./config";
import { Card, Row, SubRow, Toggle } from "./controls";
import type { Texts } from "./i18n";

/** Location consent belongs to this foreground process. No mount-time prompt
 * unless weather was already opted into and its stored fix needs renewing. */
export function WeatherSettings({ config, texts: t, onChange }: {
  config: Config; texts: Texts; onChange: (patch: Partial<Config>) => void;
}) {
  const [status, setStatus] = useState("");
  const desired = useRef(!!config.weather);
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
      if (pending.current) void cancelWeatherLocation().catch(() => {});
    };
  }, []);

  useEffect(() => {
    const wasEnabled = desired.current;
    desired.current = !!config.weather;
    if (!config.weather) {
      if (wasEnabled) generation.current += 1;
      if (pending.current) void cancelWeatherLocation().catch(() => {});
      return;
    }
    if (status && status !== "locating") return;
    if (!config.weatherLocation || Date.now() / 1000 - config.weatherLocation.updatedAt > 6 * 3600) locate();
  }, [config.weather, config.weatherLocation, locate, status]);

  const toggle = (enabled: boolean) => {
    generation.current += 1;
    desired.current = enabled;
    setStatus("");
    onChange(enabled ? { weather: true } : { weather: false, weatherLocation: null });
    if (enabled) locate();
    else if (pending.current) void cancelWeatherLocation().catch(() => {});
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
    <SubRow label={t.weatherLocation} hint={message} open={!!config.weather} stacked>
      <div className="weather-settings-actions" aria-live="polite">
        <span className="sr-only" role="status">{message}</span>
        <button type="button" className="push" disabled={status === "locating"}
          onClick={() => { generation.current += 1; locate(); }}>{t.weatherUpdateLocation}</button>
        {status && status !== "locating" ? <button type="button" className="push"
          onClick={() => void openWeatherLocationSettings().catch(() => setStatus("unavailable"))}>{t.weatherLocationPrivacy}</button> : null}
        <button type="button" className="weather-credit"
          onClick={() => void openWeatherAttribution().catch(() => setStatus("unavailable"))}>{t.weatherAttribution}</button>
      </div>
    </SubRow>
  </Card>;
}
