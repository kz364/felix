import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ChevronDown } from "lucide-react";
import { useSettings } from "../../../hooks/useSettings";
import { ApiKeyField } from "../PostProcessingSettingsApi/ApiKeyField";
import { BaseUrlField } from "../PostProcessingSettingsApi/BaseUrlField";
import { ChatGptAccount } from "../voice-control/AssistantSettings";

/** Providers shown up front, and what each can do in Handy. */
const MAIN: Record<string, "speech" | "cleanup"> = {
  openai: "speech",
  groq: "speech",
  anthropic: "cleanup",
  openrouter: "cleanup",
};
/** Not accounts: they run on this Mac or through the ChatGPT sign-in. */
const NOT_ACCOUNTS = new Set(["local", "apple_intelligence", "chatgpt"]);

/** One place to connect the cloud: the ChatGPT sign-in and API keys. */
export const Accounts: React.FC = () => {
  const { t } = useTranslation();
  const { settings, updatePostProcessApiKey, updatePostProcessBaseUrl } =
    useSettings();
  const [showMore, setShowMore] = useState(false);
  const providers = (settings?.post_process_providers ?? []).filter(
    (p) => !NOT_ACCOUNTS.has(p.id),
  );
  const keys = settings?.post_process_api_keys ?? {};
  const main = providers.filter((p) => p.id in MAIN);
  const more = providers.filter((p) => !(p.id in MAIN));

  const row = (p: (typeof providers)[number]) => {
    const key = keys[p.id] ?? "";
    const connected = key.trim() !== "";
    const uses =
      MAIN[p.id] === "speech"
        ? t("settings.models.accounts.usesSpeech")
        : t("settings.models.accounts.usesCleanup");
    return (
      <div
        key={p.id}
        className="flex flex-wrap items-center justify-between gap-2 px-4 py-2.5"
      >
        <div className="min-w-0">
          <div className="flex items-center gap-1.5 text-sm font-medium">
            {p.label}
            {connected && (
              <Check
                className="h-3.5 w-3.5 text-success"
                aria-label={t("settings.models.accounts.connected")}
              />
            )}
          </div>
          <div className="text-sm text-text/60">{uses}</div>
        </div>
        <div className="flex items-center gap-2">
          {p.allow_base_url_edit && (
            <BaseUrlField
              value={p.base_url}
              onBlur={(v) => {
                const url = v.trim();
                if (url && url !== p.base_url)
                  void updatePostProcessBaseUrl(p.id, url);
              }}
              placeholder={t("settings.postProcessing.api.baseUrl.placeholder")}
              disabled={false}
            />
          )}
          <ApiKeyField
            value={key}
            onBlur={(v) => {
              if (v.trim() !== key)
                void updatePostProcessApiKey(p.id, v.trim());
            }}
            placeholder={t("settings.models.accounts.keyPlaceholder")}
            disabled={false}
          />
        </div>
      </div>
    );
  };

  return (
    <>
      <ChatGptAccount descriptionMode="inline" grouped />
      <div className="divide-y divide-stone/15">{main.map(row)}</div>
      {more.length > 0 && (
        <>
          <button
            type="button"
            onClick={() => setShowMore(!showMore)}
            className="flex w-full items-center gap-1.5 px-4 py-2 text-xs text-text/60 hover:text-text cursor-pointer"
          >
            <ChevronDown
              className={`h-3.5 w-3.5 transition-transform ${showMore ? "" : "-rotate-90"}`}
            />
            {t("settings.models.accounts.more", { count: more.length })}
          </button>
          {showMore && (
            <div className="divide-y divide-stone/15">{more.map(row)}</div>
          )}
        </>
      )}
    </>
  );
};
