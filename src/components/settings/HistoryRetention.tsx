import React from "react";
import { useTranslation } from "react-i18next";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";
import { useSettings } from "../../hooks/useSettings";

interface HistoryRetentionProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const TEXT_DAYS = [1, 7, 30, 90, 0];
const AUDIO_DAYS = [0, 1, 7, 30];

export const HistoryRetention: React.FC<HistoryRetentionProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const textDays = getSetting("history_retention_days") ?? 7;
    const audioDays = getSetting("recording_retention_days") ?? 0;

    const label = (days: number, zeroKey: string) =>
      days === 0
        ? t(zeroKey)
        : t("settings.history.retention.days", { count: days });

    return (
      <>
        <SettingContainer
          title={t("settings.history.retention.textTitle")}
          description={t("settings.history.retention.textDescription")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <Dropdown
            options={TEXT_DAYS.map((d) => ({
              value: String(d),
              label: label(d, "settings.history.retention.forever"),
            }))}
            selectedValue={String(textDays)}
            onSelect={(v) => updateSetting("history_retention_days", Number(v))}
            disabled={isUpdating("history_retention_days")}
          />
        </SettingContainer>
        <SettingContainer
          title={t("settings.history.retention.audioTitle")}
          description={t("settings.history.retention.audioDescription")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <Dropdown
            options={AUDIO_DAYS.map((d) => ({
              value: String(d),
              label: label(d, "settings.history.retention.noAudio"),
            }))}
            selectedValue={String(audioDays)}
            onSelect={(v) =>
              updateSetting("recording_retention_days", Number(v))
            }
            disabled={isUpdating("recording_retention_days")}
          />
        </SettingContainer>
      </>
    );
  },
);
