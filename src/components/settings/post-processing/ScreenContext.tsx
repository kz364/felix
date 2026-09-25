import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { useSettings } from "../../../hooks/useSettings";

/** Whether cleanup sees the text on screen around where the dictation goes. */
export const ScreenContext: React.FC<{ disabled?: boolean }> = ({
  disabled = false,
}) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const enabled = getSetting("screen_context") ?? true;
  return (
    <>
      <ToggleSwitch
        checked={enabled}
        onChange={(v) => updateSetting("screen_context", v)}
        isUpdating={isUpdating("screen_context")}
        disabled={disabled}
        label={t("settings.postProcessing.screenContext.label")}
        description={t("settings.postProcessing.screenContext.description")}
        descriptionMode="inline"
        grouped
      />
      {enabled && (
        <ToggleSwitch
          checked={getSetting("screen_context_online") ?? false}
          onChange={(v) => updateSetting("screen_context_online", v)}
          isUpdating={isUpdating("screen_context_online")}
          disabled={disabled}
          label={t("settings.postProcessing.screenContext.onlineLabel")}
          description={t(
            "settings.postProcessing.screenContext.onlineDescription",
          )}
          descriptionMode="inline"
          grouped
        />
      )}
    </>
  );
};
