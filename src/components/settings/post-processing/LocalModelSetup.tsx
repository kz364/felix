import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { commands, type LocalModelStatus } from "@/bindings";
import { Alert } from "../../ui/Alert";
import { Button } from "../../ui/Button";
import { SettingContainer } from "../../ui/SettingContainer";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { useSettings } from "../../../hooks/useSettings";

/** Payload of the backend's `local-model-install` event. */
interface InstallProgress {
  model: string;
  stage: "ollama" | "model" | "done" | "error";
  completed: number;
  total: number;
  message: string;
}

const formatGb = (bytes: number) => (bytes / 1e9).toFixed(1);

/**
 * Status of the local cleanup model with one-click setup (installs Ollama
 * and downloads the model), plus whether to keep it loaded in memory.
 */
export const LocalModelSetup: React.FC<{ model: string }> = ({ model }) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const [status, setStatus] = useState<LocalModelStatus | null>(null);
  const [progress, setProgress] = useState<InstallProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const keepLoaded = getSetting("local_model_keep_loaded") ?? true;

  const refresh = useCallback(async () => {
    setStatus(await commands.getLocalModelStatus());
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh, model]);

  useEffect(() => {
    const unlisten = listen<InstallProgress>("local-model-install", (e) => {
      // Installs of other models (from the Models tab) don't concern this row.
      if (e.payload.model && e.payload.model !== model) return;
      setProgress(e.payload);
      if (e.payload.stage === "done" || e.payload.stage === "error") {
        void refresh();
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [refresh, model]);

  const install = async () => {
    setError(null);
    setProgress({
      model,
      stage: "ollama",
      completed: 0,
      total: 0,
      message: "",
    });
    const result = await commands.installLocalModel(null);
    if (result.status === "error") setError(result.error);
    setProgress(null);
    void refresh();
  };

  const installing =
    status?.installing ||
    (progress !== null &&
      progress.stage !== "done" &&
      progress.stage !== "error");
  const ready = status?.runtime_installed && status?.model_installed;
  const percent =
    progress && progress.total > 0
      ? Math.min(100, (progress.completed / progress.total) * 100)
      : null;

  let description: string;
  if (!status) description = "";
  else if (ready)
    description = t("settings.postProcessing.local.ready", {
      model: status.model,
    });
  else if (!status.runtime_installed)
    description = t("settings.postProcessing.local.needsOllama", {
      model: status.model,
    });
  else
    description = t("settings.postProcessing.local.needsModel", {
      model: status.model,
    });

  return (
    <>
      <SettingContainer
        title={t("settings.postProcessing.local.title")}
        description={description}
        descriptionMode="inline"
        layout="stacked"
        grouped={true}
      >
        {installing ? (
          <div className="flex flex-col gap-1.5">
            <div className="text-xs text-text/55">
              {progress?.stage === "model"
                ? t("settings.postProcessing.local.downloadingModel", {
                    model: status?.model ?? model,
                  })
                : t("settings.postProcessing.local.downloadingOllama")}
              {percent !== null && progress && (
                <>
                  {" "}
                  {t("settings.postProcessing.local.progress", {
                    done: formatGb(progress.completed),
                    total: formatGb(progress.total),
                  })}
                </>
              )}
            </div>
            <div className="h-1.5 w-full overflow-hidden rounded-full bg-stone/20">
              <div
                className={`h-full bg-accent transition-[width] ${percent === null ? "w-1/3 animate-pulse" : ""}`}
                style={percent !== null ? { width: `${percent}%` } : undefined}
              />
            </div>
          </div>
        ) : (
          !ready &&
          status && (
            <Button variant="primary" size="md" onClick={install}>
              {status.runtime_installed
                ? t("settings.postProcessing.local.installModel", {
                    model: status.model,
                  })
                : t("settings.postProcessing.local.installAll", {
                    model: status.model,
                  })}
            </Button>
          )
        )}
      </SettingContainer>
      {error && (
        <Alert variant="error" contained>
          {error}
        </Alert>
      )}
      <ToggleSwitch
        checked={keepLoaded}
        onChange={(v) => updateSetting("local_model_keep_loaded", v)}
        isUpdating={isUpdating("local_model_keep_loaded")}
        label={t("settings.postProcessing.local.keepLoaded.label")}
        description={t("settings.postProcessing.local.keepLoaded.description")}
        descriptionMode="tooltip"
        grouped={true}
      />
      {!keepLoaded && (
        <Alert variant="warning" contained>
          {t("settings.postProcessing.local.keepLoaded.warning")}
        </Alert>
      )}
    </>
  );
};
