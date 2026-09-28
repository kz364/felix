import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Globe } from "lucide-react";
import type { AppCategory, CategorizedEntry, Formality } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";

const FORMALITIES: Formality[] = ["formal", "casual", "very_casual"];
const ALL: AppCategory[] = ["personal", "work", "email", "coding", "other"];

type MoveFn = (entry: CategorizedEntry, category: AppCategory) => void;

const EntryChip: React.FC<{ entry: CategorizedEntry; onMove: MoveFn }> = ({
  entry,
  onMove,
}) => {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);

  return (
    <div ref={ref} className="relative">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        title={entry.key}
        aria-haspopup="menu"
        aria-expanded={open}
        className="inline-flex items-center gap-1.5 rounded-full border border-stone/30 px-2.5 py-1 text-xs hover:border-stone/60 hover:bg-stone/10"
      >
        {entry.kind === "website" && (
          <Globe className="w-3 h-3 text-text/50" aria-hidden />
        )}
        <span className="max-w-40 truncate">{entry.label}</span>
        {entry.overridden && (
          <span
            className="w-1.5 h-1.5 rounded-full bg-accent"
            aria-label={t("settings.style.apps.overridden")}
          />
        )}
      </button>
      {open && (
        <div
          role="menu"
          className="absolute z-20 mt-1 min-w-48 rounded-lg border border-stone/30 bg-surface shadow-lg shadow-black/10 py-1 text-sm"
        >
          <div className="px-3 py-1 text-xs text-text/50">
            {t("settings.style.apps.moveTo")}
          </div>
          {ALL.map((category) => (
            <button
              key={category}
              type="button"
              role="menuitemradio"
              aria-checked={entry.category === category}
              onClick={() => {
                setOpen(false);
                onMove(entry, category);
              }}
              className="flex w-full items-center justify-between gap-3 px-3 py-1.5 hover:bg-stone/15"
            >
              <span>
                {t(`settings.style.categories.${category}`)}
                {category === entry.automatic && (
                  <span className="ms-1 text-xs text-text/50">
                    {t("settings.style.apps.automaticTag")}
                  </span>
                )}
              </span>
              {entry.category === category && <Check className="w-3.5 h-3.5" />}
            </button>
          ))}
        </div>
      )}
    </div>
  );
};

export const CategoryCard: React.FC<{
  category: AppCategory;
  entries: CategorizedEntry[];
  searching: boolean;
  onMove: MoveFn;
}> = ({ category, entries, searching, onMove }) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const styles = getSetting("category_styles");
  const formality: Formality = styles?.[category] ?? "formal";

  // "Everything else" holds most installed apps; unless searching, only show
  // the ones the user touched or dictated into recently.
  const shown =
    category === "other" && !searching
      ? entries.filter((e) => e.overridden || e.recent)
      : entries;
  const hidden = entries.length - shown.length;

  return (
    <section className="rounded-xl border border-stone/20 bg-surface p-4 space-y-3">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <h3 className="text-sm font-medium">
            {t(`settings.style.categories.${category}`)}
          </h3>
          <p className="mt-0.5 font-display text-sm text-text/60">
            {t(`settings.style.formality.example.${formality}`)}
          </p>
        </div>
        <div
          role="radiogroup"
          aria-label={t("settings.style.formality.title")}
          className="flex shrink-0 gap-0.5 rounded-full bg-sunken p-0.5"
        >
          {FORMALITIES.map((f) => (
            <button
              key={f}
              type="button"
              role="radio"
              aria-checked={formality === f}
              disabled={!styles || isUpdating("category_styles")}
              onClick={() =>
                styles &&
                updateSetting("category_styles", { ...styles, [category]: f })
              }
              className={`rounded-full px-2.5 py-0.5 text-xs font-medium transition-colors ${
                formality === f
                  ? "bg-surface text-text shadow-sm ring-1 ring-stone/20"
                  : "text-text/55 hover:text-text"
              }`}
            >
              {t(`settings.style.formality.levels.${f}`)}
            </button>
          ))}
        </div>
      </div>

      {shown.length > 0 ? (
        <div className="flex flex-wrap gap-1.5">
          {shown.map((entry) => (
            <EntryChip
              key={`${entry.kind}:${entry.key}`}
              entry={entry}
              onMove={onMove}
            />
          ))}
        </div>
      ) : (
        <p className="text-sm text-text/40">
          {searching
            ? t("settings.style.apps.noMatches")
            : t("settings.style.apps.empty")}
        </p>
      )}
      {hidden > 0 && (
        <p className="text-sm text-text/50">
          {t("settings.style.apps.moreApps", { count: hidden })}
        </p>
      )}
    </section>
  );
};
