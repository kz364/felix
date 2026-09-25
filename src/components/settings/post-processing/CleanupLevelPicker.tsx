import React from "react";
import { useTranslation } from "react-i18next";
import type { CleanupLevel } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";

const LEVELS: CleanupLevel[] = ["none", "light", "medium"];

export const CleanupLevelPicker: React.FC<{ disabled?: boolean }> = ({
  disabled = false,
}) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const current = (getSetting("cleanup_level") ?? "light") as CleanupLevel;

  return (
    <div className="p-4 space-y-3">
      <p className="text-sm text-text/70">
        {t("settings.postProcessing.cleanup.intro")}
      </p>
      <div className="grid grid-cols-1 sm:grid-cols-3 gap-3">
        {LEVELS.map((level) => {
          const selected = level === current;
          return (
            <button
              key={level}
              type="button"
              disabled={disabled || isUpdating("cleanup_level")}
              onClick={() => updateSetting("cleanup_level", level)}
              aria-pressed={selected}
              className={`text-start rounded-xl border p-3 flex flex-col gap-2 transition-colors disabled:opacity-50 ${
                selected
                  ? "border-accent bg-accent/5 ring-[3px] ring-accent/15"
                  : "border-stone/25 bg-surface hover:border-stone/50"
              }`}
            >
              <span className="text-sm font-medium">
                {t(`settings.postProcessing.cleanup.levels.${level}.title`)}
              </span>
              <span className="text-xs leading-snug text-text/60">
                {t(
                  `settings.postProcessing.cleanup.levels.${level}.description`,
                )}
              </span>
              <span className="mt-auto rounded-lg bg-sunken p-2 font-display text-[13px] leading-snug text-text/80">
                {t(`settings.postProcessing.cleanup.levels.${level}.example`)}
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
};
