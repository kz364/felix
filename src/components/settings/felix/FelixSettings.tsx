import React from "react";
import { useTranslation } from "react-i18next";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { AssistantSettings } from "../voice-control/AssistantSettings";
import { useSettings } from "../../../hooks/useSettings";

/** The assistant's own page: off by default, since it acts for the user. */
export const FelixSettings: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const enabled = getSetting("assistant_enabled") ?? false;
  const agentActions = getSetting("agent_actions_enabled") ?? false;
  const autoSend = getSetting("agent_auto_send") ?? true;

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup
        title={t("settings.voiceControl.assistant.groupTitle")}
        description={t("settings.voiceControl.assistant.groupDescription")}
      >
        <p className="px-4 pt-3 text-sm text-mid-gray">
          {t("settings.voiceControl.assistant.warning")}
        </p>
        <AssistantSettings descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>
      {enabled && (
        <SettingsGroup
          title={t("settings.voiceControl.assistant.computer.groupTitle")}
          description={t(
            "settings.voiceControl.assistant.computer.groupDescription",
          )}
        >
          <ToggleSwitch
            checked={agentActions}
            onChange={(v) => updateSetting("agent_actions_enabled", v)}
            isUpdating={isUpdating("agent_actions_enabled")}
            label={t("settings.voiceControl.assistant.computer.toggle.label")}
            description={t(
              "settings.voiceControl.assistant.computer.toggle.description",
            )}
            descriptionMode="tooltip"
            grouped={true}
          />
          {agentActions && (
            <ToggleSwitch
              checked={autoSend}
              onChange={(v) => updateSetting("agent_auto_send", v)}
              isUpdating={isUpdating("agent_auto_send")}
              label={t(
                "settings.voiceControl.assistant.computer.autoSend.label",
              )}
              description={t(
                "settings.voiceControl.assistant.computer.autoSend.description",
              )}
              descriptionMode="tooltip"
              grouped={true}
            />
          )}
        </SettingsGroup>
      )}
    </div>
  );
};
