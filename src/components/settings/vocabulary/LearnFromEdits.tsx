import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { useSettings } from "../../../hooks/useSettings";

/** Add words you correct after a paste to the vocabulary (edit_learning.rs). */
export const LearnFromEdits: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  return (
    <ToggleSwitch
      checked={getSetting("learn_from_edits") ?? true}
      onChange={(v) => updateSetting("learn_from_edits", v)}
      isUpdating={isUpdating("learn_from_edits")}
      label={t("settings.vocabulary.learned.learnFromEdits.label")}
      description={t("settings.vocabulary.learned.learnFromEdits.description")}
      descriptionMode="tooltip"
      grouped={true}
    />
  );
};
