import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import type { AutoSubmitKey, VoiceTrigger } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { useOsType } from "../../../hooks/useOsType";
import { Input } from "../../ui/Input";
import { Button } from "../../ui/Button";
import { Dropdown } from "../../ui/Dropdown";
import { SettingContainer } from "../../ui/SettingContainer";

interface TriggerPhrasesProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const normalizePhrase = (phrase: string) =>
  phrase
    .replace(/[^\p{L}\p{N}' ]/gu, " ")
    .replace(/\s+/g, " ")
    .trim()
    .toLowerCase();

export const TriggerPhrases: React.FC<TriggerPhrasesProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const osType = useOsType();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const [phrase, setPhrase] = useState("");
    const [key, setKey] = useState<AutoSubmitKey>("enter");

    const triggers = getSetting("voice_triggers") || [];
    const updating = isUpdating("voice_triggers");
    const normalized = normalizePhrase(phrase);

    const keyLabel = (value: AutoSubmitKey) => {
      switch (value) {
        case "enter":
          return t("settings.advanced.autoSubmit.options.enter");
        case "ctrl_enter":
          return t("settings.advanced.autoSubmit.options.ctrlEnter");
        case "cmd_enter":
          return osType === "macos"
            ? t("settings.advanced.autoSubmit.options.cmdEnter")
            : t("settings.advanced.autoSubmit.options.superEnter");
      }
    };

    const keyOptions = (["enter", "ctrl_enter", "cmd_enter"] as const).map(
      (value) => ({ value, label: keyLabel(value) }),
    );

    const handleAdd = () => {
      if (!normalized) return;
      if (triggers.some((trigger) => trigger.phrase === normalized)) {
        toast.error(
          t("settings.voiceControl.triggers.duplicate", {
            phrase: normalized,
          }),
        );
        return;
      }
      const next: VoiceTrigger[] = [...triggers, { phrase: normalized, key }];
      updateSetting("voice_triggers", next);
      setPhrase("");
    };

    const handleRemove = (toRemove: string) => {
      updateSetting(
        "voice_triggers",
        triggers.filter((trigger) => trigger.phrase !== toRemove),
      );
    };

    return (
      <>
        <SettingContainer
          title={t("settings.voiceControl.triggers.title")}
          description={t("settings.voiceControl.triggers.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <div className="flex items-center gap-2">
            <Input
              type="text"
              className="max-w-36"
              value={phrase}
              onChange={(e) => setPhrase(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  handleAdd();
                }
              }}
              placeholder={t("settings.voiceControl.triggers.placeholder")}
              variant="compact"
              disabled={updating}
            />
            <Dropdown
              options={keyOptions}
              selectedValue={key}
              onSelect={(value) => setKey(value as AutoSubmitKey)}
              disabled={updating}
            />
            <Button
              onClick={handleAdd}
              disabled={!normalized || updating}
              variant="primary"
              size="md"
            >
              {t("settings.voiceControl.triggers.add")}
            </Button>
          </div>
        </SettingContainer>
        {triggers.length > 0 && (
          <div
            className={`px-4 p-2 ${grouped ? "" : "rounded-xl border border-stone/20 bg-surface"} flex flex-wrap gap-1`}
          >
            {triggers.map((trigger) => (
              <Button
                key={trigger.phrase}
                onClick={() => handleRemove(trigger.phrase)}
                disabled={updating}
                variant="secondary"
                size="sm"
                className="inline-flex items-center gap-1 cursor-pointer"
                aria-label={t("settings.voiceControl.triggers.remove", {
                  phrase: trigger.phrase,
                })}
              >
                <span>
                  {t("settings.voiceControl.triggers.item", {
                    phrase: trigger.phrase,
                    key: keyLabel(trigger.key),
                  })}
                </span>
                <svg
                  className="w-3 h-3"
                  fill="none"
                  stroke="currentColor"
                  viewBox="0 0 24 24"
                >
                  <path
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    strokeWidth={2}
                    d="M6 18L18 6M6 6l12 12"
                  />
                </svg>
              </Button>
            ))}
          </div>
        )}
      </>
    );
  },
);
