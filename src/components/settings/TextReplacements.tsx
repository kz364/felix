import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import type { TextReplacement } from "@/bindings";
import { useSettings } from "../../hooks/useSettings";
import { Input } from "../ui/Input";
import { Button } from "../ui/Button";
import { SettingContainer } from "../ui/SettingContainer";

interface TextReplacementsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const TextReplacements: React.FC<TextReplacementsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const [from, setFrom] = useState("");
    const [to, setTo] = useState("");

    const replacements = getSetting("text_replacements") || [];
    const updating = isUpdating("text_replacements");
    const fromTrimmed = from.replace(/\s+/g, " ").trim();
    const toTrimmed = to.trim();

    const handleAdd = () => {
      if (!fromTrimmed) return;
      if (
        replacements.some(
          (r) => r.from.toLowerCase() === fromTrimmed.toLowerCase(),
        )
      ) {
        toast.error(
          t("settings.advanced.textReplacements.duplicate", {
            from: fromTrimmed,
          }),
        );
        return;
      }
      const next: TextReplacement[] = [
        ...replacements,
        { from: fromTrimmed, to: toTrimmed },
      ];
      updateSetting("text_replacements", next);
      setFrom("");
      setTo("");
    };

    const handleRemove = (toRemove: string) => {
      updateSetting(
        "text_replacements",
        replacements.filter((r) => r.from !== toRemove),
      );
    };

    const onEnter = (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        handleAdd();
      }
    };

    return (
      <>
        <SettingContainer
          title={t("settings.advanced.textReplacements.title")}
          description={t("settings.advanced.textReplacements.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <div className="flex items-center gap-2">
            <Input
              type="text"
              className="max-w-32"
              value={from}
              onChange={(e) => setFrom(e.target.value)}
              onKeyDown={onEnter}
              placeholder={t(
                "settings.advanced.textReplacements.fromPlaceholder",
              )}
              variant="compact"
              disabled={updating}
            />
            <span className="text-text/60">→</span>
            <Input
              type="text"
              className="max-w-32"
              value={to}
              onChange={(e) => setTo(e.target.value)}
              onKeyDown={onEnter}
              placeholder={t(
                "settings.advanced.textReplacements.toPlaceholder",
              )}
              variant="compact"
              disabled={updating}
            />
            <Button
              onClick={handleAdd}
              disabled={!fromTrimmed || updating}
              variant="primary"
              size="md"
            >
              {t("settings.advanced.textReplacements.add")}
            </Button>
          </div>
        </SettingContainer>
        {replacements.length > 0 && (
          <div
            className={`px-4 p-2 ${grouped ? "" : "rounded-lg border border-mid-gray/20"} flex flex-wrap gap-1`}
          >
            {replacements.map((r) => (
              <Button
                key={r.from}
                onClick={() => handleRemove(r.from)}
                disabled={updating}
                variant="secondary"
                size="sm"
                className="inline-flex items-center gap-1 cursor-pointer"
                aria-label={t("settings.advanced.textReplacements.remove", {
                  from: r.from,
                })}
              >
                <span>
                  {t("settings.advanced.textReplacements.item", {
                    from: r.from,
                    to: r.to,
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
