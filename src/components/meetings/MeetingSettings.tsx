import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands, type MeetingLlm, type MeetingTranscriber } from "@/bindings";
import { useSettings } from "../../hooks/useSettings";
import { useModelStore } from "../../stores/modelStore";
import { Dropdown } from "../ui/Dropdown";
import { Input } from "../ui/Input";
import { SettingContainer } from "../ui/SettingContainer";
import { Slider } from "../ui/Slider";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { Button } from "../ui/Button";
import { toast } from "sonner";
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

/** The user's name: always their voice's label, always spelled right. */
const UserName: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const saved = getSetting("user_name") ?? "";
  const [name, setName] = useState(saved);
  useEffect(() => setName(saved), [saved]);
  const save = () => {
    if (name.trim() !== saved) updateSetting("user_name", name.trim());
  };
  return (
    <SettingContainer
      title={t("meetings.settings.userName.title")}
      description={t("meetings.settings.userName.description")}
      descriptionMode="tooltip"
      grouped
      layout="horizontal"
    >
      <Input
        type="text"
        value={name}
        placeholder={t("meetings.settings.userName.placeholder")}
        onChange={(e) => setName(e.target.value)}
        onBlur={save}
        onKeyDown={(e) => e.key === "Enter" && save()}
        disabled={isUpdating("user_name")}
        className="w-48"
      />
    </SettingContainer>
  );
};

/** A text setting saved when the field is left or Enter is pressed. */
const TextSetting: React.FC<{
  setting:
    | "notion_token"
    | "notion_parent"
    | "notion_share_parent"
    | "slack_token"
    | "slack_channel"
    | "slack_webhook";
  titleKey: string;
  descriptionKey: string;
  placeholderKey: string;
  secret?: boolean;
}> = ({ setting, titleKey, descriptionKey, placeholderKey, secret }) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const saved = getSetting(setting) ?? "";
  const [value, setValue] = useState(saved);
  useEffect(() => setValue(saved), [saved]);
  const save = () => {
    if (value.trim() !== saved) updateSetting(setting, value.trim());
  };
  return (
    <SettingContainer
      title={t(titleKey)}
      description={t(descriptionKey)}
      descriptionMode={secret ? "inline" : "tooltip"}
      grouped
      layout="horizontal"
    >
      <Input
        type={secret ? "password" : "text"}
        value={value}
        placeholder={t(placeholderKey)}
        onChange={(e) => setValue(e.target.value)}
        onBlur={save}
        onKeyDown={(e) => e.key === "Enter" && save()}
        disabled={isUpdating(setting)}
        autoComplete="off"
        spellCheck={false}
        className="w-48"
      />
    </SettingContainer>
  );
};

/** Saving meetings to Notion, private by default. */
const NotionSettings: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  return (
    <>
      <ToggleSwitch
        checked={getSetting("notion_sync") ?? false}
        onChange={(v) => updateSetting("notion_sync", v)}
        isUpdating={isUpdating("notion_sync")}
        label={t("meetings.settings.notion.sync.label")}
        description={t("meetings.settings.notion.sync.description")}
        descriptionMode="tooltip"
        grouped
      />
      <TextSetting
        setting="notion_token"
        titleKey="meetings.settings.notion.token.title"
        descriptionKey="meetings.settings.notion.token.description"
        placeholderKey="meetings.settings.notion.token.placeholder"
        secret
      />
      <TextSetting
        setting="notion_parent"
        titleKey="meetings.settings.notion.parent.title"
        descriptionKey="meetings.settings.notion.parent.description"
        placeholderKey="meetings.settings.notion.parent.placeholder"
      />
      <TextSetting
        setting="notion_share_parent"
        titleKey="meetings.settings.notion.shareParent.title"
        descriptionKey="meetings.settings.notion.shareParent.description"
        placeholderKey="meetings.settings.notion.shareParent.placeholder"
      />
    </>
  );
};

/** Sending each meeting's notes to Slack, e.g. to the user's agent: by
 * the user's own Slack app (summary and transcript as files), or by a
 * workflow webhook (the notes as text) until that's set up. */
const SlackSettings: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const copyManifest = async () => {
    await navigator.clipboard.writeText(await commands.slackAppManifest());
    toast.success(t("meetings.settings.slack.app.copied"));
  };
  return (
    <>
      <ToggleSwitch
        checked={getSetting("slack_send") ?? false}
        onChange={(v) => updateSetting("slack_send", v)}
        isUpdating={isUpdating("slack_send")}
        label={t("meetings.settings.slack.send.label")}
        description={t("meetings.settings.slack.send.description")}
        descriptionMode="tooltip"
        grouped
      />
      <SettingContainer
        title={t("meetings.settings.slack.app.title")}
        description={t("meetings.settings.slack.app.description")}
        descriptionMode="inline"
        grouped
        layout="horizontal"
      >
        <Button size="sm" variant="secondary" onClick={copyManifest}>
          {t("meetings.settings.slack.app.copy")}
        </Button>
      </SettingContainer>
      <TextSetting
        setting="slack_token"
        titleKey="meetings.settings.slack.token.title"
        descriptionKey="meetings.settings.slack.token.description"
        placeholderKey="meetings.settings.slack.token.placeholder"
        secret
      />
      <TextSetting
        setting="slack_channel"
        titleKey="meetings.settings.slack.channel.title"
        descriptionKey="meetings.settings.slack.channel.description"
        placeholderKey="meetings.settings.slack.channel.placeholder"
      />
      <TextSetting
        setting="slack_webhook"
        titleKey="meetings.settings.slack.webhook.title"
        descriptionKey="meetings.settings.slack.webhook.description"
        placeholderKey="meetings.settings.slack.webhook.placeholder"
        secret
      />
    </>
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
      <UserName />
      <SlackSettings />
      <NotionSettings />
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
