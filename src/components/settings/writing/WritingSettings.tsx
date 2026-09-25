import React from "react";
import { useTranslation } from "react-i18next";
import { PageHeader } from "../../ui/PageHeader";
import {
  CleanupSections,
  PromptShortcut,
} from "../post-processing/PostProcessingSettings";
import { StyleSettings } from "../style/StyleSettings";

/** How your words come out: cleanup, your instructions and tone by app. */
export const WritingSettings: React.FC = () => {
  const { t } = useTranslation();
  return (
    <div className="max-w-2xl w-full mx-auto space-y-6">
      <PageHeader
        title={t("settings.writing.title")}
        description={t("settings.writing.description")}
      />
      <CleanupSections />
      <StyleSettings />
      <PromptShortcut />
    </div>
  );
};
