import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface ContextAwarePasteProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const ContextAwarePaste: React.FC<ContextAwarePasteProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    return (
      <ToggleSwitch
        checked={getSetting("context_aware_paste") ?? true}
        onChange={(v) => updateSetting("context_aware_paste", v)}
        isUpdating={isUpdating("context_aware_paste")}
        label={t("settings.advanced.contextAwarePaste.label")}
        description={t("settings.advanced.contextAwarePaste.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);
