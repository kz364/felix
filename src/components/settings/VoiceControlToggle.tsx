import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface VoiceControlToggleProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const VoiceControlToggle: React.FC<VoiceControlToggleProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("voice_control_enabled") ?? true;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(value) => updateSetting("voice_control_enabled", value)}
        isUpdating={isUpdating("voice_control_enabled")}
        label={t("settings.voiceControl.toggle.label")}
        description={t("settings.voiceControl.toggle.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);
