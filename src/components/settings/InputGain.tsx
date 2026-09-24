import React from "react";
import { useTranslation } from "react-i18next";
import { Slider } from "../ui/Slider";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface InputGainProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const InputGain: React.FC<InputGainProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const gainDb = getSetting("input_gain_db") ?? 0;
    const autoGain = getSetting("auto_gain_enabled") ?? true;

    return (
      <>
        <ToggleSwitch
          checked={autoGain}
          onChange={(enabled) => updateSetting("auto_gain_enabled", enabled)}
          isUpdating={isUpdating("auto_gain_enabled")}
          label={t("settings.sound.autoGain.label")}
          description={t("settings.sound.autoGain.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        />
        <Slider
          value={gainDb}
          onChange={(value: number) => updateSetting("input_gain_db", value)}
          min={-20}
          max={30}
          step={1}
          label={t("settings.sound.inputGain.title")}
          description={t("settings.sound.inputGain.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
          formatValue={(value) =>
            `${value > 0 ? "+" : ""}${Math.round(value)} dB`
          }
          onReset={() => updateSetting("input_gain_db", 0)}
          isResetting={isUpdating("input_gain_db")}
        />
      </>
    );
  },
);
