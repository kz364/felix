import React, { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  commands,
  type AppCategory,
  type AppRule,
  type InstalledApp,
} from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { Button } from "../../ui/Button";
import { Dropdown } from "../../ui/Dropdown";
import { Input } from "../../ui/Input";
import { Select } from "../../ui/Select";
import { SettingContainer } from "../../ui/SettingContainer";
import { CATEGORIES } from "./FormalityByCategory";

const sameRule = (a: AppRule, b: AppRule) =>
  a.kind === b.kind && a.key === b.key;

export const AppAssignments: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const rules = getSetting("app_rules") || [];
  const recent = getSetting("recent_contexts") || [];
  const updating = isUpdating("app_rules");

  const [apps, setApps] = useState<InstalledApp[]>([]);
  const [appPath, setAppPath] = useState<string | null>(null);
  const [site, setSite] = useState("");

  useEffect(() => {
    commands.listInstalledApps().then(setApps);
  }, []);

  const categoryOptions = CATEGORIES.map((c) => ({
    value: c,
    label: t(`settings.style.categories.${c}`),
  }));

  const appOptions = useMemo(
    () =>
      apps
        .filter((a) => a.bundle_id)
        .map((a) => ({ value: a.path, label: a.name })),
    [apps],
  );

  const save = (next: AppRule[]) => updateSetting("app_rules", next);

  const assign = (rule: AppRule) => {
    const exists = rules.some((r) => sameRule(r, rule));
    save(
      exists
        ? rules.map((r) => (sameRule(r, rule) ? rule : r))
        : [...rules, rule],
    );
  };

  const addApp = () => {
    const app = apps.find((a) => a.path === appPath);
    if (!app?.bundle_id) return;
    assign({
      kind: "app",
      key: app.bundle_id,
      label: app.name,
      category: "work",
    });
    setAppPath(null);
  };

  const addSite = () => {
    const domain = site
      .trim()
      .replace(/^https?:\/\//, "")
      .split(/[/?#]/)[0]
      .replace(/^www\./, "")
      .toLowerCase();
    if (!domain.includes(".")) {
      toast.error(t("settings.style.apps.invalidSite"));
      return;
    }
    assign({ kind: "website", key: domain, label: domain, category: "work" });
    setSite("");
  };

  // Group rows by category so the list reads like the four buckets.
  const sorted = [...rules].sort(
    (a, b) =>
      CATEGORIES.indexOf(a.category) - CATEGORIES.indexOf(b.category) ||
      a.label.localeCompare(b.label),
  );

  return (
    <>
      {recent.length > 0 && (
        <div className="px-4 py-3 space-y-2 border-b border-mid-gray/20">
          <div className="flex items-center justify-between">
            <span className="text-sm font-medium">
              {t("settings.style.apps.recentTitle")}
            </span>
            <button
              type="button"
              className="text-xs text-text/60 hover:text-text"
              onClick={() => updateSetting("recent_contexts", [])}
            >
              {t("settings.style.apps.clearRecent")}
            </button>
          </div>
          {recent.map((entry) => (
            <div
              key={`${entry.kind}:${entry.key}`}
              className="flex items-center justify-between gap-2"
            >
              <span className="text-sm truncate" title={entry.key}>
                {entry.label}
                <span className="ms-2 text-xs text-text/50">
                  {t(`settings.style.apps.kind.${entry.kind}`)}
                </span>
              </span>
              <div className="flex gap-1 shrink-0">
                {CATEGORIES.map((category) => (
                  <Button
                    key={category}
                    variant="secondary"
                    size="sm"
                    disabled={updating}
                    onClick={() => assign({ ...entry, category })}
                  >
                    {t(`settings.style.categories.${category}`)}
                  </Button>
                ))}
              </div>
            </div>
          ))}
        </div>
      )}

      <SettingContainer
        title={t("settings.style.apps.addApp")}
        description={t("settings.style.apps.addAppDescription")}
        descriptionMode="tooltip"
        grouped={true}
      >
        <div className="flex items-center gap-2">
          <div className="min-w-52">
            <Select
              value={appPath}
              options={appOptions}
              onChange={(value) => setAppPath(value)}
              placeholder={t("settings.style.apps.pickApp")}
            />
          </div>
          <Button
            variant="primary"
            size="md"
            disabled={!appPath || updating}
            onClick={addApp}
          >
            {t("settings.style.apps.add")}
          </Button>
        </div>
      </SettingContainer>

      <SettingContainer
        title={t("settings.style.apps.addSite")}
        description={t("settings.style.apps.addSiteDescription")}
        descriptionMode="tooltip"
        grouped={true}
      >
        <div className="flex items-center gap-2">
          <Input
            type="text"
            className="max-w-52"
            value={site}
            onChange={(e) => setSite(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                addSite();
              }
            }}
            placeholder="mail.google.com"
            variant="compact"
            disabled={updating}
          />
          <Button
            variant="primary"
            size="md"
            disabled={!site.trim() || updating}
            onClick={addSite}
          >
            {t("settings.style.apps.add")}
          </Button>
        </div>
      </SettingContainer>

      <div className="divide-y divide-mid-gray/20">
        {sorted.map((rule) => (
          <div
            key={`${rule.kind}:${rule.key}`}
            className="flex items-center justify-between gap-2 px-4 py-2"
          >
            <div className="min-w-0">
              <div className="text-sm truncate">{rule.label}</div>
              <div className="text-xs text-text/50 truncate">
                {t(`settings.style.apps.kind.${rule.kind}`)} · {rule.key}
              </div>
            </div>
            <div className="flex items-center gap-2 shrink-0">
              <Dropdown
                options={categoryOptions}
                selectedValue={rule.category}
                onSelect={(value) =>
                  assign({ ...rule, category: value as AppCategory })
                }
                disabled={updating}
              />
              <button
                type="button"
                className="text-text/50 hover:text-text p-1"
                aria-label={t("settings.style.apps.remove", {
                  name: rule.label,
                })}
                onClick={() => save(rules.filter((r) => !sameRule(r, rule)))}
              >
                <svg
                  className="w-3.5 h-3.5"
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
              </button>
            </div>
          </div>
        ))}
      </div>
    </>
  );
};
