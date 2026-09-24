import React, { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { commands, type AppAlias, type InstalledApp } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { Input } from "../../ui/Input";
import { Button } from "../../ui/Button";
import { Select } from "../../ui/Select";
import { SettingContainer } from "../../ui/SettingContainer";
import { ToggleSwitch } from "../../ui/ToggleSwitch";

interface AppSwitcherProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const normalizePhrase = (phrase: string) =>
  phrase
    .replace(/[^\p{L}\p{N}' ]/gu, " ")
    .replace(/\s+/g, " ")
    .trim()
    .toLowerCase();

const appNameFromPath = (path: string) =>
  path
    .split("/")
    .pop()
    ?.replace(/\.app$/, "") ?? path;

export const AppSwitcher: React.FC<AppSwitcherProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const [apps, setApps] = useState<InstalledApp[]>([]);
    const [phrase, setPhrase] = useState("");
    const [appPath, setAppPath] = useState<string | null>(null);

    const enabled = getSetting("app_switch_enabled") ?? true;
    const anyInstalled = getSetting("app_switch_any_installed") ?? true;
    const aliases = getSetting("app_aliases") || [];
    const updating = isUpdating("app_aliases");
    const normalized = normalizePhrase(phrase);

    useEffect(() => {
      commands.listInstalledApps().then(setApps);
    }, []);

    const appOptions = useMemo(
      () => apps.map((app) => ({ value: app.path, label: app.name })),
      [apps],
    );

    const handleAdd = () => {
      if (!normalized || !appPath) return;
      if (aliases.some((alias) => alias.phrase === normalized)) {
        toast.error(
          t("settings.voiceControl.appSwitcher.duplicate", {
            phrase: normalized,
          }),
        );
        return;
      }
      const next: AppAlias[] = [
        ...aliases,
        { phrase: normalized, app_path: appPath },
      ];
      updateSetting("app_aliases", next);
      setPhrase("");
      setAppPath(null);
    };

    const handleRemove = (toRemove: string) => {
      updateSetting(
        "app_aliases",
        aliases.filter((alias) => alias.phrase !== toRemove),
      );
    };

    return (
      <>
        <ToggleSwitch
          checked={enabled}
          onChange={(value) => updateSetting("app_switch_enabled", value)}
          isUpdating={isUpdating("app_switch_enabled")}
          label={t("settings.voiceControl.appSwitcher.toggleLabel")}
          description={t("settings.voiceControl.appSwitcher.toggleDescription")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        />
        {enabled && (
          <>
            <ToggleSwitch
              checked={anyInstalled}
              onChange={(value) =>
                updateSetting("app_switch_any_installed", value)
              }
              isUpdating={isUpdating("app_switch_any_installed")}
              label={t("settings.voiceControl.appSwitcher.anyInstalledLabel")}
              description={t(
                "settings.voiceControl.appSwitcher.anyInstalledDescription",
              )}
              descriptionMode={descriptionMode}
              grouped={grouped}
            />
            <SettingContainer
              title={t("settings.voiceControl.appSwitcher.aliasesTitle")}
              description={t(
                "settings.voiceControl.appSwitcher.aliasesDescription",
              )}
              descriptionMode={descriptionMode}
              grouped={grouped}
              layout="stacked"
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
                  placeholder={t(
                    "settings.voiceControl.appSwitcher.phrasePlaceholder",
                  )}
                  variant="compact"
                  disabled={updating}
                />
                <div className="min-w-48 flex-1">
                  <Select
                    value={appPath}
                    options={appOptions}
                    onChange={(value) => setAppPath(value)}
                    placeholder={t(
                      "settings.voiceControl.appSwitcher.appPlaceholder",
                    )}
                    disabled={updating}
                  />
                </div>
                <Button
                  onClick={handleAdd}
                  disabled={!normalized || !appPath || updating}
                  variant="primary"
                  size="md"
                >
                  {t("settings.voiceControl.appSwitcher.add")}
                </Button>
              </div>
            </SettingContainer>
            {aliases.length > 0 && (
              <div
                className={`px-4 p-2 ${grouped ? "" : "rounded-lg border border-mid-gray/20"} flex flex-wrap gap-1`}
              >
                {aliases.map((alias) => (
                  <Button
                    key={alias.phrase}
                    onClick={() => handleRemove(alias.phrase)}
                    disabled={updating}
                    variant="secondary"
                    size="sm"
                    className="inline-flex items-center gap-1 cursor-pointer"
                    aria-label={t("settings.voiceControl.appSwitcher.remove", {
                      phrase: alias.phrase,
                    })}
                  >
                    <span>
                      {t("settings.voiceControl.appSwitcher.item", {
                        phrase: alias.phrase,
                        app: appNameFromPath(alias.app_path),
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
        )}
      </>
    );
  },
);
