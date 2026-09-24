import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";
import { useSettings } from "../../hooks/useSettings";

interface ResultPopupProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const SECONDS = [3, 5, 10, 20, 0];

/** Show the dictation in a card when no text field is focused. */
export const ResultPopup: React.FC<ResultPopupProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const enabled = getSetting("result_popup_enabled") ?? true;
    const seconds = getSetting("result_popup_seconds") ?? 5;
    return (
      <>
        <ToggleSwitch
          checked={enabled}
          onChange={(v) => updateSetting("result_popup_enabled", v)}
          isUpdating={isUpdating("result_popup_enabled")}
          label={t("settings.advanced.resultPopup.label")}
          description={t("settings.advanced.resultPopup.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        />
        {enabled && (
          <SettingContainer
            title={t("settings.advanced.resultPopup.timeoutTitle")}
            description={t("settings.advanced.resultPopup.timeoutDescription")}
            descriptionMode={descriptionMode}
            grouped={grouped}
          >
            <Dropdown
              options={SECONDS.map((s) => ({
                value: String(s),
                label:
                  s === 0
                    ? t("settings.advanced.resultPopup.never")
                    : t("settings.advanced.resultPopup.seconds", { count: s }),
              }))}
              selectedValue={String(seconds)}
              onSelect={(v) => updateSetting("result_popup_seconds", Number(v))}
              disabled={isUpdating("result_popup_seconds")}
            />
          </SettingContainer>
        )}
      </>
    );
  },
);
