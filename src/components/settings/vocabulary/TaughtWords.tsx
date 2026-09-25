import React, { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { commands, type TaughtWord } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { Button } from "../../ui/Button";

/** Taught words and what the current model makes of their saved clips. */
export const TaughtWords: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, refreshSettings } = useSettings();
  const taught = getSetting("taught_words") || [];
  const modelId = getSetting("selected_model") || "";

  // Re-checks after a model change run in the background.
  useEffect(() => {
    const unlisten = listen("taught-words-updated", () => refreshSettings());
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [refreshSettings]);

  if (taught.length === 0) return null;

  const toggle = async (word: TaughtWord, variant: string, on: boolean) => {
    const entry = word.by_model.find((m) => m.model_id === modelId);
    if (!entry) return;
    const excluded = on
      ? entry.excluded.filter((v) => v !== variant)
      : [...entry.excluded, variant];
    await commands.teachSetExcluded(word.word, excluded);
    await refreshSettings();
  };

  return (
    <div className="px-4 py-3 space-y-3">
      <div className="flex items-center justify-between">
        <span className="text-sm font-medium">
          {t("settings.vocabulary.taught.title")}
        </span>
        <Button
          variant="secondary"
          size="sm"
          onClick={() => commands.teachRecheck(true)}
        >
          {t("settings.vocabulary.taught.recheckAll")}
        </Button>
      </div>
      {taught.map((word) => {
        const entry = word.by_model.find((m) => m.model_id === modelId);
        return (
          <div
            key={word.word}
            className="rounded-xl border border-stone/20 bg-surface p-3 space-y-2"
          >
            <div className="flex items-center justify-between gap-2">
              <div className="text-sm">
                <span className="font-medium">{word.word}</span>
                <span className="ms-2 text-xs text-text/50">
                  {t("settings.vocabulary.taught.clips", {
                    count: word.clips.length,
                  })}
                </span>
              </div>
              <button
                type="button"
                className="text-xs text-text/60 hover:text-text"
                onClick={async () => {
                  await commands.teachDeleteWord(word.word);
                  await refreshSettings();
                }}
              >
                {t("settings.vocabulary.taught.delete")}
              </button>
            </div>
            {!entry ? (
              <p className="text-xs text-text/60">
                {t("settings.vocabulary.taught.checking")}
              </p>
            ) : entry.variants.length === 0 ? (
              <p className="text-xs text-success">
                {t("settings.vocabulary.taught.recognized")}
              </p>
            ) : (
              <div className="space-y-1">
                <p className="text-xs text-text/60">
                  {t("settings.vocabulary.taught.rewrites")}
                </p>
                {entry.variants.map((variant) => (
                  <label
                    key={variant}
                    className="flex items-center gap-2 text-sm cursor-pointer"
                  >
                    <input
                      type="checkbox"
                      checked={!entry.excluded.includes(variant)}
                      onChange={(e) => toggle(word, variant, e.target.checked)}
                    />
                    <span>
                      {t("settings.vocabulary.teach.variantItem", {
                        variant,
                        word: word.word,
                      })}
                    </span>
                  </label>
                ))}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
};
