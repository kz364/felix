import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface FocusMessageBoxProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const FocusMessageBox: React.FC<FocusMessageBoxProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    return (
      <ToggleSwitch
        checked={getSetting("focus_message_box") ?? true}
        onChange={(v) => updateSetting("focus_message_box", v)}
        isUpdating={isUpdating("focus_message_box")}
        label={t("settings.advanced.focusMessageBox.label")}
        description={t("settings.advanced.focusMessageBox.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);
