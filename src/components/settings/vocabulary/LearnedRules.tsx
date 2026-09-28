import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Sparkles, X } from "lucide-react";
import { commands, type Learned } from "@/bindings";

/** Everything the rules file holds, each entry deletable. */
export const LearnedRules: React.FC<{
  version: number;
  onChange: () => void;
}> = ({ version, onChange }) => {
  const { t } = useTranslation();
  const [learned, setLearned] = useState<Learned | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    commands.learnedRules().then(setLearned);
  }, []);
  useEffect(load, [load, version]);

  const forget = async (kind: string, index: number) => {
    const result = await commands.forgetRule(kind, index);
    if (result.status === "error") setError(result.error);
    else {
      setError(null);
      onChange();
    }
  };

  if (!learned) return null;
  const empty =
    learned.words.length === 0 &&
    learned.corrections.length === 0 &&
    learned.soundalikes.length === 0;

  const remove = (kind: string, index: number, label: string) => (
    <button
      type="button"
      onClick={() => forget(kind, index)}
      aria-label={t("settings.vocabulary.learned.remove", { label })}
      title={t("settings.vocabulary.learned.remove", { label })}
      className="shrink-0 rounded p-0.5 text-text/40 hover:text-error hover:bg-error/10 cursor-pointer"
    >
      <X className="h-3.5 w-3.5" />
    </button>
  );

  return (
    <div className="px-4 py-3 space-y-4">
      {learned.error && (
        <p className="text-sm text-error">
          {t("settings.vocabulary.learned.fileError", {
            error: learned.error,
          })}
        </p>
      )}
      {error && <p className="text-sm text-error">{error}</p>}
      {empty && (
        <p className="text-sm text-text/50">
          {t("settings.vocabulary.learned.empty")}
        </p>
      )}

      {learned.words.length > 0 && (
        <section className="space-y-2">
          <h4 className="text-sm font-medium text-text/70">
            {t("settings.vocabulary.learned.words")}
          </h4>
          <div className="flex flex-wrap gap-1.5">
            {learned.words.map((word, i) => (
              <span
                key={`${word}-${i}`}
                className="inline-flex items-center gap-1 rounded-full border border-stone/30 bg-stone/10 ps-2.5 pe-1 py-0.5 text-sm"
              >
                {learned.auto_learned.includes(word) && (
                  <span
                    title={t("settings.vocabulary.learned.autoLearned")}
                    aria-label={t("settings.vocabulary.learned.autoLearned")}
                  >
                    <Sparkles className="h-3 w-3 text-accent" />
                  </span>
                )}
                {word}
                {remove("word", i, word)}
              </span>
            ))}
          </div>
        </section>
      )}

      {learned.corrections.length > 0 && (
        <section className="space-y-1.5">
          <h4 className="text-sm font-medium text-text/70">
            {t("settings.vocabulary.learned.corrections")}
          </h4>
          {learned.corrections.map((c, i) => (
            <div
              key={`${c.from}-${i}`}
              className="flex items-center justify-between gap-2 rounded-md px-2 py-1 text-sm hover:bg-stone/10"
            >
              <span className="min-w-0 truncate">
                {t("settings.vocabulary.learned.correction", {
                  from: c.from,
                  to: c.to,
                })}
              </span>
              {remove("correction", i, c.from)}
            </div>
          ))}
        </section>
      )}

      {learned.soundalikes.length > 0 && (
        <section className="space-y-1.5">
          <h4 className="text-sm font-medium text-text/70">
            {t("settings.vocabulary.learned.soundalikes")}
          </h4>
          {learned.soundalikes.map((s, i) => (
            <div
              key={`${s.heard}-${i}`}
              className="flex items-start justify-between gap-2 rounded-md px-2 py-1 text-sm hover:bg-stone/10"
            >
              <div className="min-w-0">
                <div>
                  {t("settings.vocabulary.learned.soundalike", {
                    heard: s.heard,
                    word: s.word,
                  })}
                </div>
                <div className="text-xs text-text/50">
                  {[
                    (s.compounds ?? []).join(", "),
                    (s.name_in_apps ?? []).length > 0
                      ? t("settings.vocabulary.learned.inApps", {
                          apps: (s.name_in_apps ?? []).join(", "),
                        })
                      : "",
                  ]
                    .filter(Boolean)
                    .join(" · ")}
                </div>
              </div>
              {remove("soundalike", i, s.heard)}
            </div>
          ))}
        </section>
      )}
    </div>
  );
};
