import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands, type MeetingLlm, type MeetingTranscriber } from "@/bindings";
import { useSettings } from "../../hooks/useSettings";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";
import { Slider } from "../ui/Slider";
import { ToggleSwitch } from "../ui/ToggleSwitch";

/** How meetings are transcribed, told apart, tidied and summarised. */
export const MeetingSettings: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const [defaultPrompt, setDefaultPrompt] = useState("");
  const [prompt, setPrompt] = useState(
    getSetting("meeting_summary_prompt") ?? "",
  );

  useEffect(() => {
    commands.defaultMeetingSummaryPrompt().then(setDefaultPrompt);
  }, []);

  const transcriber = getSetting("meeting_transcriber") ?? "local";
  const llm = getSetting("meeting_llm") ?? "chatgpt";
  const autoGain = getSetting("meeting_auto_gain") ?? true;

  return (
    <>
      <SettingContainer
        title={t("meetings.settings.transcriber.title")}
        description={t("meetings.settings.transcriber.description")}
        descriptionMode="tooltip"
        grouped
      >
        <Dropdown
          options={[
            {
              value: "local",
              label: t("meetings.settings.transcriber.local"),
            },
            {
              value: "openai",
              label: t("meetings.settings.transcriber.openai"),
            },
            { value: "groq", label: t("meetings.settings.transcriber.groq") },
          ]}
          selectedValue={transcriber}
          onSelect={(v) =>
            updateSetting("meeting_transcriber", v as MeetingTranscriber)
          }
          disabled={isUpdating("meeting_transcriber")}
        />
      </SettingContainer>
      <ToggleSwitch
        checked={getSetting("meeting_diarize") ?? true}
        onChange={(v) => updateSetting("meeting_diarize", v)}
        isUpdating={isUpdating("meeting_diarize")}
        label={t("meetings.settings.diarize.label")}
        description={t("meetings.settings.diarize.description")}
        descriptionMode="tooltip"
        grouped
      />
      <ToggleSwitch
        checked={autoGain}
        onChange={(v) => updateSetting("meeting_auto_gain", v)}
        isUpdating={isUpdating("meeting_auto_gain")}
        label={t("meetings.settings.autoGain.label")}
        description={t("meetings.settings.autoGain.description")}
        descriptionMode="tooltip"
        grouped
      />
      <Slider
        value={getSetting("meeting_input_boost_db") ?? 0}
        onChange={(v) => updateSetting("meeting_input_boost_db", v)}
        min={0}
        max={24}
        step={1}
        label={t("meetings.settings.boost.label")}
        description={t("meetings.settings.boost.description")}
        descriptionMode="tooltip"
        grouped
        formatValue={(v) => `+${v.toFixed(0)} dB`}
      />
      <SettingContainer
        title={t("meetings.settings.llm.title")}
        description={t("meetings.settings.llm.description")}
        descriptionMode="tooltip"
        grouped
      >
        <Dropdown
          options={[
            { value: "chatgpt", label: t("meetings.settings.llm.chatgpt") },
            { value: "cleanup", label: t("meetings.settings.llm.cleanup") },
          ]}
          selectedValue={llm}
          onSelect={(v) => updateSetting("meeting_llm", v as MeetingLlm)}
          disabled={isUpdating("meeting_llm")}
        />
      </SettingContainer>
      <ToggleSwitch
        checked={getSetting("meeting_cleanup") ?? true}
        onChange={(v) => updateSetting("meeting_cleanup", v)}
        isUpdating={isUpdating("meeting_cleanup")}
        label={t("meetings.settings.cleanup.label")}
        description={t("meetings.settings.cleanup.description")}
        descriptionMode="tooltip"
        grouped
      />
      <SettingContainer
        title={t("meetings.settings.prompt.title")}
        description={t("meetings.settings.prompt.description")}
        descriptionMode="inline"
        layout="stacked"
        grouped
      >
        <textarea
          value={prompt}
          rows={6}
          onChange={(e) => setPrompt(e.target.value)}
          onBlur={() => {
            if (prompt !== (getSetting("meeting_summary_prompt") ?? "")) {
              updateSetting("meeting_summary_prompt", prompt);
            }
          }}
          placeholder={defaultPrompt}
          className="w-full resize-y rounded-lg border border-stone/25 bg-surface px-3 py-2 text-sm leading-relaxed outline-none focus:border-accent/60 placeholder:text-text/35"
        />
      </SettingContainer>
    </>
  );
};
