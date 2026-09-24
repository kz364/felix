import React from "react";
import { useTranslation } from "react-i18next";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { TriggerPhrases } from "./TriggerPhrases";
import { AppSwitcher } from "./AppSwitcher";
import { VoiceControlToggle } from "../VoiceControlToggle";

export const VoiceControlSettings: React.FC = () => {
  const { t } = useTranslation();

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup>
        <VoiceControlToggle descriptionMode="inline" grouped={true} />
      </SettingsGroup>
      <SettingsGroup
        title={t("settings.voiceControl.triggers.groupTitle")}
        description={t("settings.voiceControl.triggers.groupDescription")}
      >
        <TriggerPhrases descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>
      <SettingsGroup
        title={t("settings.voiceControl.appSwitcher.groupTitle")}
        description={t("settings.voiceControl.appSwitcher.groupDescription")}
      >
        <AppSwitcher descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>
    </div>
  );
};
