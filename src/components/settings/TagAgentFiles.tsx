import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

/** File names become @mentions for Claude Code and Codex (agent_files.rs). */
export const TagAgentFiles: React.FC = React.memo(() => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  return (
    <ToggleSwitch
      checked={getSetting("tag_agent_files") ?? true}
      onChange={(v) => updateSetting("tag_agent_files", v)}
      isUpdating={isUpdating("tag_agent_files")}
      label={t("settings.advanced.tagAgentFiles.label")}
      description={t("settings.advanced.tagAgentFiles.description")}
      descriptionMode="tooltip"
      grouped={true}
    />
  );
});
