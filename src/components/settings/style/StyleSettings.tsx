import React, { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { useSettingsStore } from "../../../stores/settingsStore";
import { FormalityByCategory } from "./FormalityByCategory";
import { AppAssignments } from "./AppAssignments";

export const StyleSettings: React.FC = () => {
  const { t } = useTranslation();
  const refreshSettings = useSettingsStore((s) => s.refreshSettings);
  // Dictations add unassigned apps to "recently used" in the backend.
  useEffect(() => {
    refreshSettings();
  }, [refreshSettings]);
  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup
        title={t("settings.style.formality.title")}
        description={t("settings.style.formality.description")}
      >
        <FormalityByCategory />
      </SettingsGroup>
      <SettingsGroup
        title={t("settings.style.apps.title")}
        description={t("settings.style.apps.description")}
      >
        <AppAssignments />
      </SettingsGroup>
    </div>
  );
};
