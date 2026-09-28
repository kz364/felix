import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

/** One message per sentence in personal messengers (stacked.rs). */
export const StackedMessages: React.FC = React.memo(() => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  return (
    <ToggleSwitch
      checked={getSetting("stacked_messages") ?? false}
      onChange={(v) => updateSetting("stacked_messages", v)}
      isUpdating={isUpdating("stacked_messages")}
      label={t("settings.advanced.stackedMessages.label")}
      description={t("settings.advanced.stackedMessages.description")}
      descriptionMode="tooltip"
      grouped={true}
    />
  );
});
