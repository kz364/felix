import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { commands } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { SettingContainer } from "../../ui/SettingContainer";
import { Dropdown } from "../../ui/Dropdown";
import { Input } from "../../ui/Input";
import { Textarea } from "../../ui/Textarea";
import { Button } from "../../ui/Button";

interface AssistantSettingsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const MODELS = [
  { value: "gpt-6-astra", label: "GPT-6 Astra" },
  { value: "gpt-6-sol", label: "GPT-6 Sol" },
  { value: "gpt-6-luna", label: "GPT-6 Luna" },
];
const EFFORTS = ["none", "low", "medium", "high"] as const;

/** ChatGPT sign-in state and button. */
export const ChatGptAccount: React.FC<AssistantSettingsProps> = ({
  descriptionMode,
  grouped,
}) => {
  const { t } = useTranslation();
  // undefined = still loading, null = signed out, "" = signed in, no email.
  const [account, setAccount] = useState<string | null | undefined>();
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    commands.chatgptAccount().then(setAccount);
  }, []);

  const signIn = async () => {
    setBusy(true);
    const result = await commands.chatgptSignIn();
    setBusy(false);
    if (result.status === "ok") {
      setAccount(result.data);
    } else {
      toast.error(
        t("settings.voiceControl.assistant.account.failed", {
          error: result.error,
        }),
      );
    }
  };

  const signOut = async () => {
    await commands.chatgptSignOut();
    setAccount(null);
  };

  const signedIn = account !== null && account !== undefined;
  return (
    <SettingContainer
      title={t("settings.voiceControl.assistant.account.title")}
      description={
        signedIn
          ? account
            ? t("settings.voiceControl.assistant.account.signedInAs", {
                email: account,
              })
            : t("settings.voiceControl.assistant.account.signedIn")
          : t("settings.voiceControl.assistant.account.description")
      }
      descriptionMode={signedIn ? "inline" : descriptionMode}
      grouped={grouped}
    >
      {signedIn ? (
        <Button variant="secondary" size="sm" onClick={signOut}>
          {t("settings.voiceControl.assistant.account.signOut")}
        </Button>
      ) : (
        <Button
          size="sm"
          onClick={signIn}
          disabled={busy || account === undefined}
        >
          {busy
            ? t("settings.voiceControl.assistant.account.signingIn")
            : t("settings.voiceControl.assistant.account.signIn")}
        </Button>
      )}
    </SettingContainer>
  );
};

/** The voice assistant: on/off, sign-in, name, model and notes. */
export const AssistantSettings: React.FC<AssistantSettingsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const enabled = getSetting("assistant_enabled") ?? false;
    const name = getSetting("assistant_name") ?? "Felix";
    const model = getSetting("assistant_model") ?? "gpt-6-sol";
    const effort = getSetting("assistant_effort") ?? "low";
    const notes = getSetting("assistant_notes") ?? "";
    const [nameDraft, setNameDraft] = useState(name);
    const [notesDraft, setNotesDraft] = useState(notes);

    useEffect(() => setNameDraft(name), [name]);
    useEffect(() => setNotesDraft(notes), [notes]);

    const saveName = () => {
      const trimmed = nameDraft.trim();
      if (trimmed === name) return;
      if (!trimmed || /\s/.test(trimmed)) {
        toast.error(t("settings.voiceControl.assistant.name.invalid"));
        setNameDraft(name);
        return;
      }
      updateSetting("assistant_name", trimmed);
    };

    const modelOptions = MODELS.some((m) => m.value === model)
      ? MODELS
      : [...MODELS, { value: model, label: model }];

    return (
      <>
        <ToggleSwitch
          checked={enabled}
          onChange={(v) => updateSetting("assistant_enabled", v)}
          isUpdating={isUpdating("assistant_enabled")}
          label={t("settings.voiceControl.assistant.toggleLabel")}
          description={t("settings.voiceControl.assistant.toggleDescription")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        />
        {enabled && (
          <>
            <ChatGptAccount
              descriptionMode={descriptionMode}
              grouped={grouped}
            />
            <SettingContainer
              title={t("settings.voiceControl.assistant.name.title")}
              description={t(
                "settings.voiceControl.assistant.name.description",
              )}
              descriptionMode={descriptionMode}
              grouped={grouped}
            >
              <Input
                value={nameDraft}
                onChange={(e) => setNameDraft(e.target.value)}
                onBlur={saveName}
                onKeyDown={(e) => e.key === "Enter" && saveName()}
                disabled={isUpdating("assistant_name")}
                variant="compact"
                className="w-32"
              />
            </SettingContainer>
            <SettingContainer
              title={t("settings.voiceControl.assistant.model.title")}
              description={t(
                "settings.voiceControl.assistant.model.description",
              )}
              descriptionMode={descriptionMode}
              grouped={grouped}
            >
              <Dropdown
                options={modelOptions}
                selectedValue={model}
                onSelect={(v) => updateSetting("assistant_model", v)}
                disabled={isUpdating("assistant_model")}
              />
            </SettingContainer>
            <SettingContainer
              title={t("settings.voiceControl.assistant.effort.title")}
              description={t(
                "settings.voiceControl.assistant.effort.description",
              )}
              descriptionMode={descriptionMode}
              grouped={grouped}
            >
              <Dropdown
                options={EFFORTS.map((value) => ({
                  value,
                  label: t(
                    `settings.voiceControl.assistant.effort.options.${value}`,
                  ),
                }))}
                selectedValue={effort}
                onSelect={(v) => updateSetting("assistant_effort", v)}
                disabled={isUpdating("assistant_effort")}
              />
            </SettingContainer>
            <SettingContainer
              title={t("settings.voiceControl.assistant.notes.title")}
              description={t(
                "settings.voiceControl.assistant.notes.description",
              )}
              descriptionMode="inline"
              grouped={grouped}
              layout="stacked"
            >
              <Textarea
                aria-label={t("settings.voiceControl.assistant.notes.title")}
                value={notesDraft}
                placeholder={t(
                  "settings.voiceControl.assistant.notes.placeholder",
                )}
                onChange={(e) => setNotesDraft(e.target.value)}
                onBlur={() =>
                  notesDraft !== notes &&
                  updateSetting("assistant_notes", notesDraft)
                }
                variant="compact"
                className="w-full font-normal"
              />
            </SettingContainer>
          </>
        )}
      </>
    );
  },
);
