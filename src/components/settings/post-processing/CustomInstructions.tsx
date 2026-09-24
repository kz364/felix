import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { AppCategory, CategoryInstructions } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { Textarea } from "../../ui/Textarea";

const CATEGORIES: AppCategory[] = ["personal", "work", "email", "other"];
const EMPTY: CategoryInstructions = {
  personal: "",
  work: "",
  email: "",
  other: "",
};

/** Saves on blur so typing doesn't write settings on every keystroke. */
const InstructionBox: React.FC<{
  value: string;
  onSave: (value: string) => void;
  placeholder: string;
  label: string;
  disabled?: boolean;
}> = ({ value, onSave, placeholder, label, disabled }) => {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  return (
    <Textarea
      aria-label={label}
      value={draft}
      placeholder={placeholder}
      disabled={disabled}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={() => draft !== value && onSave(draft)}
      variant="compact"
      className="w-full font-normal"
    />
  );
};

export const CustomInstructions: React.FC<{ disabled?: boolean }> = ({
  disabled = false,
}) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting } = useSettings();
  const global = getSetting("custom_instructions") ?? "";
  const perCategory = getSetting("category_instructions") ?? EMPTY;
  const configured = CATEGORIES.filter((c) => perCategory[c].trim()).length;

  return (
    <div className="p-4 space-y-3">
      <InstructionBox
        value={global}
        onSave={(v) => updateSetting("custom_instructions", v)}
        placeholder={t("settings.postProcessing.instructions.placeholder")}
        label={t("settings.postProcessing.instructions.everywhere")}
        disabled={disabled}
      />
      <details className="group">
        <summary className="cursor-pointer text-sm text-text/70 hover:text-text select-none">
          {t("settings.postProcessing.instructions.perCategory")}
          {configured > 0 && (
            <span className="ms-1 text-xs text-text/50">
              {t("settings.postProcessing.instructions.configured", {
                count: configured,
              })}
            </span>
          )}
        </summary>
        <div className="mt-3 space-y-3">
          {CATEGORIES.map((category) => (
            <div key={category} className="space-y-1">
              <div className="text-xs font-medium text-text/70">
                {t(`settings.style.categories.${category}`)}
              </div>
              <InstructionBox
                value={perCategory[category]}
                onSave={(v) =>
                  updateSetting("category_instructions", {
                    ...perCategory,
                    [category]: v,
                  })
                }
                placeholder={t(
                  `settings.postProcessing.instructions.examples.${category}`,
                )}
                label={t(`settings.style.categories.${category}`)}
                disabled={disabled}
              />
            </div>
          ))}
        </div>
      </details>
    </div>
  );
};
