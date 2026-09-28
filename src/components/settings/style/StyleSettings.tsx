import React, { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  commands,
  type AppCategory,
  type AppRule,
  type CategorizedEntry,
} from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { Input } from "../../ui/Input";
import { Button } from "../../ui/Button";
import { CategoryCard } from "./CategoryCard";

export const CATEGORIES: AppCategory[] = [
  "personal",
  "work",
  "email",
  "coding",
  "other",
];

/** Tone by app: which apps and sites count as personal, work or email. */
export const StyleSettings: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting } = useSettings();
  const overrides = getSetting("app_rules") || [];
  const [entries, setEntries] = useState<CategorizedEntry[]>([]);
  const [query, setQuery] = useState("");
  const [site, setSite] = useState("");
  const [siteCategory, setSiteCategory] = useState<AppCategory>("work");

  const reload = useCallback(() => {
    commands.listAppCategories().then(setEntries);
  }, []);
  useEffect(reload, [reload]);

  const setCategory = async (
    entry: Pick<CategorizedEntry, "kind" | "key" | "label" | "automatic">,
    category: AppCategory,
  ) => {
    const rest = overrides.filter(
      (r) => !(r.kind === entry.kind && r.key === entry.key),
    );
    const next: AppRule[] =
      category === entry.automatic
        ? rest
        : [
            ...rest,
            {
              kind: entry.kind,
              key: entry.key,
              label: entry.label,
              category,
            },
          ];
    await updateSetting("app_rules", next);
    reload();
  };

  const addSite = async () => {
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
    const existing = entries.find(
      (e) => e.kind === "website" && e.key === domain,
    );
    await setCategory(
      existing ?? {
        kind: "website",
        key: domain,
        label: domain,
        automatic: "other",
      },
      siteCategory,
    );
    setSite("");
  };

  const needle = query.trim().toLowerCase();
  const visible = useMemo(
    () =>
      needle
        ? entries.filter(
            (e) =>
              e.label.toLowerCase().includes(needle) ||
              e.key.toLowerCase().includes(needle),
          )
        : entries,
    [entries, needle],
  );

  return (
    <section className="space-y-3">
      <div className="px-1">
        <h2 className="text-sm font-medium text-text/70">
          {t("settings.style.title")}
        </h2>
        <p className="mt-0.5 text-sm leading-snug text-text/55">
          {t("settings.style.intro")}
        </p>
      </div>

      <Input
        type="search"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        placeholder={t("settings.style.search")}
        className="w-full"
      />

      {CATEGORIES.map((category) => (
        <CategoryCard
          key={category}
          category={category}
          entries={visible.filter((e) => e.category === category)}
          searching={needle.length > 0}
          onMove={setCategory}
        />
      ))}

      <div className="rounded-xl border border-stone/20 bg-surface p-4 space-y-2">
        <div className="text-sm font-medium">
          {t("settings.style.apps.addSite")}
        </div>
        <p className="text-sm text-text/60">
          {t("settings.style.apps.addSiteDescription")}
        </p>
        <div className="flex items-center gap-2">
          <Input
            type="text"
            className="flex-1"
            value={site}
            onChange={(e) => setSite(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                addSite();
              }
            }}
            placeholder="docs.google.com"
            variant="compact"
          />
          <select
            value={siteCategory}
            onChange={(e) => setSiteCategory(e.target.value as AppCategory)}
            className="h-8 rounded-lg border border-stone/30 bg-surface px-2 text-sm"
            aria-label={t("settings.style.apps.categoryFor")}
          >
            {CATEGORIES.map((c) => (
              <option key={c} value={c}>
                {t(`settings.style.categories.${c}`)}
              </option>
            ))}
          </select>
          <Button
            variant="primary"
            size="md"
            disabled={!site.trim()}
            onClick={addSite}
          >
            {t("settings.style.apps.add")}
          </Button>
        </div>
      </div>
    </section>
  );
};
