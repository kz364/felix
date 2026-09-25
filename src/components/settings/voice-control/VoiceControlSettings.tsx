import React from "react";
import { useTranslation } from "react-i18next";
import { PageHeader } from "../../ui/PageHeader";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { TriggerPhrases } from "./TriggerPhrases";
import { VoiceControlToggle } from "../VoiceControlToggle";

/** Spoken commands: trigger phrases and line breaks. */
export const VoiceCommandsSettings: React.FC = () => {
  const { t } = useTranslation();

  return (
    <div className="max-w-2xl w-full mx-auto space-y-6">
      <PageHeader
        title={t("settings.voiceControl.title")}
        description={t("settings.voiceControl.description")}
      />
      <SettingsGroup>
        <VoiceControlToggle descriptionMode="inline" grouped={true} />
      </SettingsGroup>
      <SettingsGroup
        title={t("settings.voiceControl.triggers.groupTitle")}
        description={t("settings.voiceControl.triggers.groupDescription")}
      >
        <TriggerPhrases descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>
    </div>
  );
};
