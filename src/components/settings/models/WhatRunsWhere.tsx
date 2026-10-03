import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { RefreshCcw } from "lucide-react";
import { commands, type OpenRouterModel, type ThisMac } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { useModelStore } from "@/stores/modelStore";
import { Alert } from "../../ui/Alert";
import { Dropdown } from "../../ui/Dropdown";
import { ResetButton } from "../../ui/ResetButton";
import { SettingContainer } from "../../ui/SettingContainer";
import { ModelSelect } from "../PostProcessingSettingsApi/ModelSelect";
import { usePostProcessProviderState } from "../PostProcessingSettingsApi/usePostProcessProviderState";
import { LocalModelSetup } from "../post-processing/LocalModelSetup";

/** Cloud speech-to-text providers (id, what they run; OpenRouter's is a setting). */
const SPEECH_CLOUD = [
  { id: "openai", label: "OpenAI", model: "gpt-transcribe" },
  { id: "groq", label: "Groq", model: "whisper-large-v3-turbo" },
  { id: "openrouter", label: "OpenRouter", model: "" },
  { id: "elevenlabs", label: "ElevenLabs", model: "Scribe v2" },
];
const CHATGPT_MODELS = [
  { value: "gpt-6-luna", label: "GPT-6 Luna" },
  { value: "gpt-6-sol", label: "GPT-6 Sol" },
];
const ON_THIS_MAC = new Set(["local", "apple_intelligence"]);

/** Where speech-to-text and cleanup run: this Mac or a connected account. */
export const WhatRunsWhere: React.FC = () => {
  const { t } = useTranslation();
  const { settings, refreshSettings } = useSettings();
  const { models, currentModel } = useModelStore();
  const cleanup = usePostProcessProviderState();
  const [mac, setMac] = useState<ThisMac | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [routerModels, setRouterModels] = useState<OpenRouterModel[] | null>(
    null,
  );
  const [routerError, setRouterError] = useState<string | null>(null);

  useEffect(() => {
    commands.thisMac().then(setMac);
  }, []);

  const keys = settings?.post_process_api_keys ?? {};
  const hasKey = (id: string) => (keys[id] ?? "").trim() !== "";
  const speech = settings?.transcription_provider ?? "local";
  const routerModel =
    settings?.openrouter_transcription_model ?? "microsoft/mai-transcribe-2";

  useEffect(() => {
    if (speech !== "openrouter" || routerModels) return;
    commands.openrouterTranscriptionModels().then((result) => {
      if (result.status === "ok") setRouterModels(result.data);
      else setRouterError(result.error);
    });
  }, [speech, routerModels]);
  const localName =
    models.find((m) => m.id === currentModel)?.name ??
    t("settings.models.where.noLocalModel");

  const speechOptions = [
    {
      value: "local",
      label: t("settings.models.where.thisMac", { model: localName }),
    },
    ...SPEECH_CLOUD.map((p) => ({
      value: p.id,
      label: hasKey(p.id)
        ? t("settings.models.where.cloud", {
            provider: p.label,
            model: p.model || routerModel,
          })
        : t("settings.models.where.needsKey", { provider: p.label }),
      disabled: !hasKey(p.id),
    })),
  ];

  const selectSpeech = async (value: string) => {
    const result = await commands.changeTranscriptionProviderSetting(value);
    if (result.status === "error") setError(result.error);
    else setError(null);
    await refreshSettings();
  };

  const cleanupId = cleanup.selectedProviderId;
  const selectRouterModel = async (model: string) => {
    const result =
      await commands.changeOpenrouterTranscriptionModelSetting(model);
    if (result.status === "error") setError(result.error);
    await refreshSettings();
  };
  const routerOptions = (
    routerModels ?? [{ id: routerModel, name: routerModel }]
  ).map((m) => ({ value: m.id, label: m.name }));

  const cleanupOptions = cleanup.providerOptions.map((o) => {
    const needsKey =
      !ON_THIS_MAC.has(o.value) &&
      o.value !== "chatgpt" &&
      o.value !== "custom" &&
      !hasKey(o.value);
    const where = ON_THIS_MAC.has(o.value)
      ? t("settings.models.where.onThisMac")
      : o.value === "chatgpt"
        ? t("settings.models.where.chatgptNote")
        : "";
    return {
      value: o.value,
      label: needsKey
        ? t("settings.models.where.needsKey", { provider: o.label })
        : where
          ? `${o.label} · ${where}`
          : o.label,
      disabled: needsKey && o.value !== cleanupId,
    };
  });

  const advice = !mac
    ? null
    : mac.local_cleanup
      ? t("settings.models.where.macAll", {
          chip: mac.chip,
          memory: mac.memory_gb,
        })
      : mac.local_speech
        ? t("settings.models.where.macSpeech", {
            chip: mac.chip,
            memory: mac.memory_gb,
          })
        : t("settings.models.where.macCloud", {
            chip: mac.chip,
            memory: mac.memory_gb,
          });

  return (
    <>
      {advice && <p className="px-4 pt-3 text-sm text-text/60">{advice}</p>}
      <SettingContainer
        title={t("settings.models.where.speech.title")}
        description={t("settings.models.where.speech.description")}
        descriptionMode="tooltip"
        grouped
      >
        <Dropdown
          options={speechOptions}
          selectedValue={speech}
          onSelect={selectSpeech}
        />
      </SettingContainer>
      {speech === "openrouter" && (
        <SettingContainer
          title={t("settings.models.where.openrouterModel.title")}
          description={t("settings.models.where.openrouterModel.description")}
          descriptionMode="tooltip"
          grouped
        >
          <Dropdown
            options={routerOptions}
            selectedValue={routerModel}
            onSelect={selectRouterModel}
            placeholder={
              routerModels
                ? undefined
                : t("settings.models.where.openrouterModel.loading")
            }
          />
        </SettingContainer>
      )}
      {routerError && speech === "openrouter" && (
        <Alert variant="error" contained>
          {t("settings.models.where.openrouterModel.failed", {
            error: routerError,
          })}
        </Alert>
      )}
      {error && (
        <Alert variant="error" contained>
          {error}
        </Alert>
      )}
      <SettingContainer
        title={t("settings.models.where.cleanup.title")}
        description={t("settings.models.where.cleanup.description")}
        descriptionMode="tooltip"
        grouped
      >
        <Dropdown
          options={cleanupOptions}
          selectedValue={cleanupId}
          onSelect={cleanup.handleProviderSelect}
        />
      </SettingContainer>
      {cleanupId === "chatgpt" ? (
        <SettingContainer
          title={t("settings.postProcessing.api.model.title")}
          description={t("settings.models.where.chatgptModel")}
          descriptionMode="tooltip"
          grouped
        >
          <Dropdown
            options={CHATGPT_MODELS}
            selectedValue={cleanup.model || "gpt-6-luna"}
            onSelect={cleanup.handleModelSelect}
          />
        </SettingContainer>
      ) : cleanupId !== "apple_intelligence" && cleanupId !== "local" ? (
        <SettingContainer
          title={t("settings.postProcessing.api.model.title")}
          description={t(
            "settings.postProcessing.api.model.descriptionDefault",
          )}
          descriptionMode="tooltip"
          layout="stacked"
          grouped
        >
          <div className="flex items-center gap-2">
            <ModelSelect
              value={cleanup.model}
              options={cleanup.modelOptions}
              disabled={cleanup.isModelUpdating}
              isLoading={cleanup.isFetchingModels}
              placeholder={
                cleanup.modelOptions.length > 0
                  ? t(
                      "settings.postProcessing.api.model.placeholderWithOptions",
                    )
                  : t("settings.postProcessing.api.model.placeholderNoOptions")
              }
              onSelect={cleanup.handleModelSelect}
              onCreate={cleanup.handleModelCreate}
              onBlur={() => {}}
              className="flex-1 min-w-[380px]"
            />
            <ResetButton
              onClick={cleanup.handleRefreshModels}
              disabled={cleanup.isFetchingModels}
              ariaLabel={t("settings.postProcessing.api.model.refreshModels")}
              className="flex h-10 w-10 items-center justify-center"
            >
              <RefreshCcw
                className={`h-4 w-4 ${cleanup.isFetchingModels ? "animate-spin" : ""}`}
              />
            </ResetButton>
          </div>
        </SettingContainer>
      ) : null}
      {cleanup.isLocalProvider && <LocalModelSetup model={cleanup.model} />}
      {cleanup.isAppleProvider && cleanup.appleIntelligenceUnavailable && (
        <Alert variant="error" contained>
          {t("settings.postProcessing.api.appleIntelligence.unavailable")}
        </Alert>
      )}
    </>
  );
};
