import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands, type MeetingLlm, type MeetingTranscriber } from "@/bindings";
import { useSettings } from "../../hooks/useSettings";
import { useModelStore } from "../../stores/modelStore";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";
import { Slider } from "../ui/Slider";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { X } from "lucide-react";
import {
  getLanguageLabel,
  MODEL_CAPABILITY_LANGUAGES,
} from "../../lib/constants/languages";

/** The model on this Mac that meetings use: dictation's, or one of their own. */
const MeetingModel: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const { models, currentModel } = useModelStore();
  const dictation = models.find((m) => m.id === currentModel)?.name ?? "";
  const own = models.filter(
    (m) =>
      m.is_downloaded &&
      m.engine_type === "TranscribeCpp" &&
      m.id !== currentModel,
  );
  const chosen = getSetting("meeting_model") ?? "";
  return (
    <SettingContainer
      title={t("meetings.settings.model.title")}
      description={t("meetings.settings.model.description")}
      descriptionMode="tooltip"
      grouped
    >
      <Dropdown
        options={[
          {
            value: "",
            label: t("meetings.settings.model.dictation", { model: dictation }),
          },
          ...own.map((m) => ({ value: m.id, label: m.name })),
        ]}
        selectedValue={own.some((m) => m.id === chosen) ? chosen : ""}
        onSelect={(v) => updateSetting("meeting_model", v)}
        disabled={isUpdating("meeting_model")}
      />
    </SettingContainer>
  );
};

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

  const transcriber = getSetting("meeting_transcriber") ?? "auto";
  const maxHours = getSetting("meeting_max_hours") ?? 4;
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
            { value: "auto", label: t("meetings.settings.transcriber.auto") },
            {
              value: "local",
              label: t("meetings.settings.transcriber.local"),
            },
            {
              value: "openai",
              label: t("meetings.settings.transcriber.openai"),
            },
            { value: "groq", label: t("meetings.settings.transcriber.groq") },
            {
              value: "openrouter",
              label: t("meetings.settings.transcriber.openrouter", {
                model:
                  getSetting("openrouter_transcription_model") ??
                  "microsoft/mai-transcribe-2",
              }),
            },
            {
              value: "elevenlabs",
              label: t("meetings.settings.transcriber.elevenlabs"),
            },
          ]}
          selectedValue={transcriber}
          onSelect={(v) =>
            updateSetting("meeting_transcriber", v as MeetingTranscriber)
          }
          disabled={isUpdating("meeting_transcriber")}
        />
      </SettingContainer>
      {(transcriber === "auto" || transcriber === "local") && <MeetingModel />}
      <MeetingLanguages />
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
        checked={getSetting("meeting_detect_calls") ?? true}
        onChange={(v) => updateSetting("meeting_detect_calls", v)}
        isUpdating={isUpdating("meeting_detect_calls")}
        label={t("meetings.settings.detectCalls.label")}
        description={t("meetings.settings.detectCalls.description")}
        descriptionMode="tooltip"
        grouped
      />
      <ToggleSwitch
        checked={getSetting("meeting_auto_stop") ?? true}
        onChange={(v) => updateSetting("meeting_auto_stop", v)}
        isUpdating={isUpdating("meeting_auto_stop")}
        label={t("meetings.settings.autoStop.label")}
        description={t("meetings.settings.autoStop.description")}
        descriptionMode="tooltip"
        grouped
      />
      <SettingContainer
        title={t("meetings.settings.maxHours.title")}
        description={t("meetings.settings.maxHours.description")}
        descriptionMode="tooltip"
        grouped
      >
        <Dropdown
          options={[2, 4, 8, 0].map((h) => ({
            value: String(h),
            label:
              h === 0
                ? t("meetings.settings.maxHours.none")
                : t("meetings.settings.maxHours.hours", { count: h }),
          }))}
          selectedValue={String(maxHours)}
          onSelect={(v) => updateSetting("meeting_max_hours", Number(v))}
          disabled={isUpdating("meeting_max_hours")}
        />
      </SettingContainer>
      <ToggleSwitch
        checked={getSetting("hide_from_screen_share") ?? false}
        onChange={(v) => updateSetting("hide_from_screen_share", v)}
        isUpdating={isUpdating("hide_from_screen_share")}
        label={t("meetings.settings.hideFromShare.label")}
        description={t("meetings.settings.hideFromShare.description")}
        descriptionMode="tooltip"
        grouped
      />
      <ToggleSwitch
        checked={getSetting("meeting_panel") ?? true}
        onChange={(v) => updateSetting("meeting_panel", v)}
        isUpdating={isUpdating("meeting_panel")}
        label={t("meetings.settings.panel.label")}
        description={t("meetings.settings.panel.description")}
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

/** The languages meetings are held in: none means found from each
 *  recording. Several for people who switch between them. */
const MeetingLanguages: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const chosen = getSetting("meeting_languages") ?? [];
  const set = (languages: string[]) =>
    updateSetting("meeting_languages", languages);
  const label = (code: string) => getLanguageLabel(code) ?? code;

  return (
    <SettingContainer
      title={t("meetings.settings.languages.title")}
      description={t("meetings.settings.languages.description")}
      descriptionMode="tooltip"
      grouped
    >
      <div className="flex flex-wrap items-center justify-end gap-1.5">
        {chosen.length === 0 && (
          <span className="text-sm text-text/60">
            {t("meetings.settings.languages.automatic")}
          </span>
        )}
        {chosen.map((code) => (
          <span
            key={code}
            className="inline-flex items-center gap-1 rounded-full border border-stone/25 ps-2.5 pe-1 py-0.5 text-sm"
          >
            {label(code)}
            <button
              onClick={() => set(chosen.filter((c) => c !== code))}
              disabled={isUpdating("meeting_languages")}
              title={t("meetings.settings.languages.remove", {
                language: label(code),
              })}
              className="rounded-full p-0.5 text-text/50 hover:text-text hover:bg-stone/10 cursor-pointer"
            >
              <X className="w-3 h-3" />
            </button>
          </span>
        ))}
        <Dropdown
          options={MODEL_CAPABILITY_LANGUAGES.filter(
            (l) => !chosen.includes(l.value),
          )}
          selectedValue={null}
          placeholder={t("meetings.settings.languages.add")}
          onSelect={(v) => set([...chosen, v])}
          disabled={isUpdating("meeting_languages")}
        />
      </div>
    </SettingContainer>
  );
};
