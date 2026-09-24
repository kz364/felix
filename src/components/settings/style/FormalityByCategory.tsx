import React from "react";
import { useTranslation } from "react-i18next";
import type { AppCategory, CategoryStyles, Formality } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { SettingContainer } from "../../ui/SettingContainer";

export const CATEGORIES: AppCategory[] = ["personal", "work", "email", "other"];
const FORMALITIES: Formality[] = ["formal", "casual", "very_casual"];

const DEFAULT_STYLES: CategoryStyles = {
  personal: "casual",
  work: "formal",
  email: "formal",
  other: "formal",
};

export const FormalityByCategory: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const styles = getSetting("category_styles") ?? DEFAULT_STYLES;

  const set = (category: AppCategory, formality: Formality) =>
    updateSetting("category_styles", { ...styles, [category]: formality });

  return (
    <>
      {CATEGORIES.map((category) => (
        <SettingContainer
          key={category}
          title={t(`settings.style.categories.${category}`)}
          description={t(
            `settings.style.formality.example.${styles[category]}`,
          )}
          descriptionMode="inline"
          grouped={true}
        >
          <div
            role="radiogroup"
            aria-label={t(`settings.style.categories.${category}`)}
            className="flex rounded-lg border border-mid-gray/30 overflow-hidden"
          >
            {FORMALITIES.map((formality) => {
              const selected = styles[category] === formality;
              return (
                <button
                  key={formality}
                  type="button"
                  role="radio"
                  aria-checked={selected}
                  disabled={isUpdating("category_styles")}
                  onClick={() => set(category, formality)}
                  className={`px-3 py-1.5 text-xs font-medium transition-colors ${
                    selected
                      ? "bg-logo-primary/80 text-white"
                      : "hover:bg-mid-gray/20"
                  }`}
                >
                  {t(`settings.style.formality.levels.${formality}`)}
                </button>
              );
            })}
          </div>
        </SettingContainer>
      ))}
    </>
  );
};
