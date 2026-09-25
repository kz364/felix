import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { FolderOpen } from "lucide-react";
import { commands, type BenchmarkSummary } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { SettingContainer } from "../../ui/SettingContainer";
import { Button } from "../../ui/Button";
import { BenchmarkReview } from "./BenchmarkReview";

/** Dev setting: keep raw audio and ground truth for benchmarking. */
export const BenchmarkRecording: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const enabled = getSetting("benchmark_recording") ?? false;
  const [summary, setSummary] = useState<BenchmarkSummary | null>(null);
  const [reviewing, setReviewing] = useState(false);
  const refresh = () => commands.benchmarkSummary().then(setSummary);

  useEffect(() => {
    refresh();
  }, [enabled]);

  return (
    <>
      <ToggleSwitch
        checked={enabled}
        onChange={(v) => updateSetting("benchmark_recording", v)}
        isUpdating={isUpdating("benchmark_recording")}
        label={t("settings.app.benchmark.label")}
        description={t("settings.app.benchmark.description")}
        descriptionMode="inline"
        grouped={true}
      />
      {summary && (summary.dictations > 0 || enabled) && (
        <SettingContainer
          title={t("settings.app.benchmark.saved.title")}
          description={t("settings.app.benchmark.saved.description", {
            count: summary.dictations,
            edited: summary.edited,
            size: summary.megabytes.toFixed(0),
          })}
          descriptionMode="inline"
          grouped={true}
        >
          <Button
            variant="secondary"
            size="sm"
            onClick={() => commands.openBenchmarkFolder()}
          >
            <FolderOpen className="w-3.5 h-3.5" />
            {t("settings.app.benchmark.saved.open")}
          </Button>
        </SettingContainer>
      )}
      {summary && summary.dictations > 0 && (
        <SettingContainer
          title={t("settings.app.benchmark.review.title")}
          description={t("settings.app.benchmark.review.description", {
            confirmed: summary.confirmed,
            guessed: summary.guessed,
            count: summary.dictations,
          })}
          descriptionMode="inline"
          grouped={true}
        >
          <Button
            variant="secondary"
            size="sm"
            onClick={() => setReviewing((v) => !v)}
          >
            {reviewing
              ? t("settings.app.benchmark.review.hide")
              : t("settings.app.benchmark.review.show")}
          </Button>
        </SettingContainer>
      )}
      {reviewing && <BenchmarkReview onChange={refresh} />}
    </>
  );
};
