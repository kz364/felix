import React from "react";
import { useTranslation } from "react-i18next";
import { type } from "@tauri-apps/plugin-os";
import { PageHeader } from "../../ui/PageHeader";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { useSettings } from "../../../hooks/useSettings";
import { ShortcutInput } from "../ShortcutInput";
import { ShortcutActivationSetting } from "../ShortcutActivation";
import { MicrophoneSelector } from "../MicrophoneSelector";
import { PreferredMicrophones } from "../PreferredMicrophones";
import { ChannelSelector } from "../ChannelSelector";
import { InputGain } from "../InputGain";
import { MuteWhileRecording } from "../MuteWhileRecording";
import { AudioFeedback } from "../AudioFeedback";
import { OutputDeviceSelector } from "../OutputDeviceSelector";
import { VolumeSlider } from "../VolumeSlider";
import { PasteMethodSetting } from "../PasteMethod";
import { TypingToolSetting } from "../TypingTool";
import { ClipboardHandlingSetting } from "../ClipboardHandling";
import { ContextAwarePaste } from "../ContextAwarePaste";
import { FocusMessageBox } from "../FocusMessageBox";
import { ResultPopup } from "../ResultPopup";
import { AppendTrailingSpace } from "../AppendTrailingSpace";
import { VoiceActivityDetection } from "../VoiceActivityDetection";
import { VadBackendSelector } from "../VadBackendSelector";
import { LanguageCard } from "./LanguageCard";

/** Everything about talking and getting text out: keys, mic, language, pasting. */
export const DictationSettings: React.FC = () => {
  const { t } = useTranslation();
  const { audioFeedbackEnabled } = useSettings();
  const isLinux = type() === "linux";
  return (
    <div className="max-w-2xl w-full mx-auto space-y-6">
      <PageHeader
        title={t("settings.dictation.title")}
        description={t("settings.dictation.description")}
      />
      <SettingsGroup title={t("settings.dictation.groups.shortcuts")}>
        <ShortcutInput shortcutId="transcribe" grouped={true} />
        <ShortcutActivationSetting descriptionMode="tooltip" grouped={true} />
        {/* Cancel shortcut remains hidden on Linux because of dynamic shortcut instability. */}
        {!isLinux && <ShortcutInput shortcutId="cancel" grouped={true} />}
      </SettingsGroup>
      <SettingsGroup title={t("settings.dictation.groups.microphone")}>
        <MicrophoneSelector descriptionMode="tooltip" grouped={true} />
        <PreferredMicrophones descriptionMode="tooltip" grouped={true} />
        <ChannelSelector descriptionMode="tooltip" grouped={true} />
        <InputGain descriptionMode="tooltip" grouped={true} />
        <MuteWhileRecording descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>
      <LanguageCard />
      <SettingsGroup title={t("settings.dictation.groups.pasting")}>
        <PasteMethodSetting descriptionMode="tooltip" grouped={true} />
        <TypingToolSetting descriptionMode="tooltip" grouped={true} />
        <ClipboardHandlingSetting descriptionMode="tooltip" grouped={true} />
        <ContextAwarePaste descriptionMode="tooltip" grouped={true} />
        <AppendTrailingSpace descriptionMode="tooltip" grouped={true} />
        <FocusMessageBox descriptionMode="tooltip" grouped={true} />
        <ResultPopup descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>
      <SettingsGroup title={t("settings.dictation.groups.sounds")}>
        <AudioFeedback descriptionMode="tooltip" grouped={true} />
        <OutputDeviceSelector
          descriptionMode="tooltip"
          grouped={true}
          disabled={!audioFeedbackEnabled}
        />
        <VolumeSlider disabled={!audioFeedbackEnabled} />
      </SettingsGroup>
      <SettingsGroup
        title={t("settings.dictation.groups.listening")}
        description={t("settings.dictation.groups.listeningDescription")}
      >
        <VoiceActivityDetection descriptionMode="tooltip" grouped={true} />
        <VadBackendSelector descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>
    </div>
  );
};
