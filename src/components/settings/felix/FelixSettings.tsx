import React from "react";
import { useTranslation } from "react-i18next";
import { PageHeader } from "../../ui/PageHeader";
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
  const fastMode = getSetting("agent_fast_mode") ?? false;

  return (
    <div className="max-w-2xl w-full mx-auto space-y-6">
      <PageHeader
        title={t("settings.voiceControl.assistant.pageTitle")}
        description={t("settings.voiceControl.assistant.groupDescription")}
      />
      <p className="rounded-xl bg-highlight/15 px-4 py-3 text-sm leading-snug text-text/80">
        {t("settings.voiceControl.assistant.warning")}
      </p>
      <SettingsGroup title={t("settings.voiceControl.assistant.groupTitle")}>
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
              checked={fastMode}
              onChange={(v) => updateSetting("agent_fast_mode", v)}
              isUpdating={isUpdating("agent_fast_mode")}
              label={t(
                "settings.voiceControl.assistant.computer.fastMode.label",
              )}
              description={t(
                "settings.voiceControl.assistant.computer.fastMode.description",
              )}
              descriptionMode="tooltip"
              grouped={true}
            />
          )}
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
