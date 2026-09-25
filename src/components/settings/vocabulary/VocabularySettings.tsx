import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { PageHeader } from "../../ui/PageHeader";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { LearnedRules } from "./LearnedRules";
import { MistakeReports } from "./MistakeReports";
import { ReportMistake } from "./ReportMistake";
import { TaughtWords } from "./TaughtWords";

/** Vocabulary is changed only by describing mistakes; what Handy learned
 *  from them is listed below, each entry deletable. */
export const VocabularySettings: React.FC = () => {
  const { t } = useTranslation();
  const [version, setVersion] = useState(0);
  const changed = () => setVersion((v) => v + 1);

  // Reports made to Felix by voice change the rules too.
  useEffect(() => {
    const unlisten = listen("rules-changed", changed);
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  return (
    <div className="max-w-2xl w-full mx-auto space-y-6">
      <PageHeader
        title={t("settings.vocabulary.title")}
        description={t("settings.vocabulary.description")}
      />
      <SettingsGroup
        title={t("settings.vocabulary.report.title")}
        description={t("settings.vocabulary.report.description")}
      >
        <ReportMistake onChange={changed} />
      </SettingsGroup>
      <SettingsGroup
        title={t("settings.vocabulary.learned.title")}
        description={t("settings.vocabulary.learned.description")}
      >
        <LearnedRules version={version} onChange={changed} />
        <TaughtWords />
      </SettingsGroup>
      <SettingsGroup
        title={t("settings.vocabulary.reports.title")}
        description={t("settings.vocabulary.reports.description")}
      >
        <MistakeReports version={version} />
      </SettingsGroup>
    </div>
  );
};
