import React from "react";
import { useTranslation } from "react-i18next";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { CustomWords } from "../CustomWords";
import { TextReplacements } from "../TextReplacements";
import { TeachWord } from "./TeachWord";

export const VocabularySettings: React.FC = () => {
  const { t } = useTranslation();

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup
        title={t("settings.vocabulary.biasing.title")}
        description={t("settings.vocabulary.biasing.description")}
      >
        <CustomWords descriptionMode="tooltip" grouped />
        <TeachWord descriptionMode="tooltip" grouped />
      </SettingsGroup>
      <SettingsGroup
        title={t("settings.vocabulary.rules.title")}
        description={t("settings.vocabulary.rules.description")}
      >
        <TextReplacements descriptionMode="tooltip" grouped />
      </SettingsGroup>
    </div>
  );
};
