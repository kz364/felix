import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { ask } from "@tauri-apps/plugin-dialog";
import { Check, Download, HardDrive, Loader2, Trash2 } from "lucide-react";
import { commands, type LocalModelEntry } from "@/bindings";
import Badge from "../../ui/Badge";
import { Button } from "../../ui/Button";
import { Alert } from "../../ui/Alert";
import { useSettings } from "../../../hooks/useSettings";

const LOCAL_PROVIDER_ID = "local";

/** Payload of the backend's `local-model-install` event. */
interface InstallProgress {
  model: string;
  stage: "ollama" | "model" | "done" | "error";
  completed: number;
  total: number;
  message: string;
}

/** i18n key suffix for the curated models. */
const KNOWN: Record<string, string> = {
  "qwen3.5:4b": "qwen4b",
  "qwen3.5:2b-q4_K_M": "qwen2b",
};

const formatSize = (mb: number) =>
  mb >= 1000 ? `${(mb / 1000).toFixed(1)} GB` : `${mb} MB`;

/**
 * Text models for AI cleanup, run on this Mac: download (installing Ollama
 * first if needed), switch between them, delete.
 */
export const CleanupModels: React.FC = () => {
  const { t } = useTranslation();
  const { settings, setPostProcessProvider, updatePostProcessModel } =
    useSettings();
  const [models, setModels] = useState<LocalModelEntry[]>([]);
  const [progress, setProgress] = useState<InstallProgress | null>(null);
  const [installing, setInstalling] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setModels(await commands.listLocalModels());
    setInstalling(await commands.installingLocalModel());
  }, []);

  useEffect(() => {
    void refresh();
    const unlisten = listen<InstallProgress>("local-model-install", (e) => {
      setProgress(e.payload);
      if (e.payload.stage === "done" || e.payload.stage === "error") {
        setProgress(null);
        void refresh();
      } else {
        setInstalling(e.payload.model);
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [refresh]);

  const usingLocal = settings?.post_process_provider_id === LOCAL_PROVIDER_ID;
  const activeModel = settings?.post_process_models?.[LOCAL_PROVIDER_ID] ?? "";

  const use = async (id: string) => {
    await updatePostProcessModel(LOCAL_PROVIDER_ID, id);
    if (!usingLocal) await setPostProcessProvider(LOCAL_PROVIDER_ID);
  };

  const download = async (id: string) => {
    setError(null);
    setInstalling(id);
    const result = await commands.installLocalModel(id);
    if (result.status === "error") setError(result.error);
    setInstalling(null);
    await refresh();
  };

  const remove = async (id: string) => {
    const confirmed = await ask(
      t("settings.models.cleanup.deleteConfirm", { model: id }),
      { title: t("settings.models.deleteTitle"), kind: "warning" },
    );
    if (!confirmed) return;
    const result = await commands.deleteLocalModel(id);
    if (result.status === "error") setError(result.error);
    await refresh();
  };

  return (
    <div className="space-y-3">
      {error && <Alert variant="error">{error}</Alert>}
      {models.map((model) => {
        const key = KNOWN[model.id];
        const name = key ? t(`settings.models.cleanup.${key}.name`) : model.id;
        const description = key
          ? t(`settings.models.cleanup.${key}.description`)
          : t("settings.models.cleanup.otherDescription");
        const isActive = usingLocal && activeModel === model.id;
        const isInstalling = installing === model.id;
        const clickable = !isActive && !isInstalling && installing === null;
        const percent =
          isInstalling && progress && progress.total > 0
            ? Math.min(100, (progress.completed / progress.total) * 100)
            : null;
        const onClick = () => {
          if (!clickable) return;
          if (model.installed) void use(model.id);
          else void download(model.id);
        };
        return (
          <div
            key={model.id}
            role={clickable ? "button" : undefined}
            tabIndex={clickable ? 0 : undefined}
            onClick={onClick}
            onKeyDown={(e) => {
              if (e.key === "Enter") onClick();
            }}
            className={[
              "flex flex-col rounded-xl px-4 py-3 gap-2 text-left transition-all duration-200 border-2",
              isActive ? "border-accent/50 bg-accent/10" : "border-stone/20",
              clickable
                ? "cursor-pointer hover:border-accent/50 hover:bg-stone/5 group"
                : "",
            ].join(" ")}
          >
            <div className="flex items-center gap-3 flex-wrap">
              <h3 className={`text-sm font-medium text-text transition-colors`}>
                {name}
              </h3>
              {model.recommended_for_device && !isActive && (
                <Badge variant="primary">
                  {t("settings.models.cleanup.recommendedForMac")}
                </Badge>
              )}
              {isActive && (
                <Badge variant="primary">
                  <Check className="w-3 h-3 mr-1" />
                  {t("settings.models.cleanup.active")}
                </Badge>
              )}
            </div>
            <p className="text-text/60 text-sm leading-relaxed">
              {description}
            </p>
            <hr className="w-full border-stone/20" />
            <div className="flex items-center gap-3 w-full h-5">
              <span className="text-xs text-text/50">{model.id}</span>
              <span className="flex items-center gap-1.5 ms-auto text-xs text-text/50">
                {model.installed ? (
                  <HardDrive className="w-3.5 h-3.5" />
                ) : (
                  <Download className="w-3.5 h-3.5" />
                )}
                {formatSize(model.size_mb)}
              </span>
              {model.installed && !isInstalling && (
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={(e) => {
                    e.stopPropagation();
                    void remove(model.id);
                  }}
                  className="flex items-center gap-1.5 text-accent/85 hover:text-accent hover:bg-stone/10"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                  <span>{t("common.delete")}</span>
                </Button>
              )}
            </div>
            {isInstalling && (
              <div className="w-full mt-1">
                <div className="w-full h-1.5 bg-stone/20 rounded-full overflow-hidden">
                  <div
                    className={`h-full bg-accent rounded-full transition-all duration-300 ${percent === null ? "w-1/3 animate-pulse" : ""}`}
                    style={
                      percent !== null ? { width: `${percent}%` } : undefined
                    }
                  />
                </div>
                <div className="flex items-center gap-1.5 text-xs mt-1 text-text/50">
                  <Loader2 className="w-3 h-3 animate-spin" />
                  {progress?.stage === "ollama"
                    ? t("settings.postProcessing.local.downloadingOllama")
                    : percent !== null
                      ? t("modelSelector.downloading", {
                          percentage: Math.round(percent),
                        })
                      : t("settings.postProcessing.local.downloadingModel", {
                          model: model.id,
                        })}
                </div>
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
};
