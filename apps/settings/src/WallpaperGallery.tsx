import { useEffect, useRef, useState } from "react";
import { Card } from "./controls";
import { applyWallpaper, loadWallpaperSelection } from "./config";
import { WALLPAPERS } from "./wallpaper-catalog";

interface WallpaperTexts {
  wallpaperGalleryTitle: string;
  wallpaperApplyLabel: string;
  wallpaperApplying: string;
  wallpaperApplyError: string;
  wallpaperNames: readonly string[];
}

export function WallpaperGallery({ texts, onApplied }: { texts: WallpaperTexts; onApplied: (preview: string) => void }) {
  const [selected, setSelected] = useState<number | null>(null);
  const [pending, setPending] = useState<number | null>(null);
  const [failed, setFailed] = useState(false);
  const busy = useRef(false);
  const revision = useRef(0);
  const mounted = useRef(true);

  useEffect(() => {
    let alive = true;
    mounted.current = true;
    const current = revision.current;
    loadWallpaperSelection().then((id) => {
      if (alive && revision.current === current) setSelected(id);
    }).catch(() => { /* An unavailable read must not prevent an explicit choice. */ });
    return () => { alive = false; mounted.current = false; };
  }, []);

  const choose = async (id: number, preview: string) => {
    if (busy.current) return;
    busy.current = true;
    revision.current += 1;
    setPending(id);
    setFailed(false);
    try {
      await applyWallpaper(id, preview);
      onApplied(preview);
      if (mounted.current) setSelected(id);
    } catch {
      if (mounted.current) setFailed(true);
    } finally {
      busy.current = false;
      if (mounted.current) setPending(null);
    }
  };

  return <Card title={texts.wallpaperGalleryTitle}>
    <div className="wallpaper-grid">
      {WALLPAPERS.map(({ id, preview, width, height }) => {
        const name = texts.wallpaperNames[id - 1];
        return <button type="button" className="wallpaper-choice" key={id}
          data-wallpaper-id={id} title={name}
          aria-label={texts.wallpaperApplyLabel.replace("{name}", name)}
          aria-pressed={selected === id} aria-busy={pending === id || undefined}
          disabled={pending !== null} onClick={() => choose(id, preview)}>
          <span className="wallpaper-image">
            <img src={preview} width={width} height={height} loading="lazy" decoding="async" alt="" />
            {pending === id ? <span className="wallpaper-pending" aria-hidden="true" /> : null}
            {selected === id ? <span className="wallpaper-check" aria-hidden="true">
              <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="2"><path d="m4 8 2.5 2.5L12 5" /></svg>
            </span> : null}
            <span className="wallpaper-name" aria-hidden="true">{name}</span>
          </span>
        </button>;
      })}
    </div>
    {pending !== null || failed ? <p className="wallpaper-status" role={failed ? "alert" : "status"}>
      {failed ? texts.wallpaperApplyError : texts.wallpaperApplying}
    </p> : null}
  </Card>;
}
