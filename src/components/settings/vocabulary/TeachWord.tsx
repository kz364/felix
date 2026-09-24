import React, { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { commands, type TeachTake, type TextReplacement } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { Button } from "../../ui/Button";
import { Dialog } from "../../ui/Dialog";
import { Select } from "../../ui/Select";
import { SettingContainer } from "../../ui/SettingContainer";

interface TeachWordProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

// Three normal takes, then two whispered: the pipeline has to cope with both.
const TAKE_MODES = [
  "normal",
  "normal",
  "normal",
  "whisper",
  "whisper",
] as const;

type Phase = "idle" | "recording" | "processing";

const escapeRegex = (text: string) =>
  text.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&").replace(/\s+/g, "\\s+");

export const TeachWord: React.FC<TeachWordProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting } = useSettings();
    const customWords = getSetting("custom_words") || [];
    const replacements = getSetting("text_replacements") || [];

    const [word, setWord] = useState<string | null>(null);
    const [open, setOpen] = useState(false);
    const [phase, setPhase] = useState<Phase>("idle");
    const [takes, setTakes] = useState<TeachTake[]>([]);
    const [selected, setSelected] = useState<Set<string>>(new Set());

    const done = takes.length >= TAKE_MODES.length;
    const mode = TAKE_MODES[Math.min(takes.length, TAKE_MODES.length - 1)];

    // Unique mis-hearings across takes, common English words unticked.
    const variants = useMemo(() => {
      const seen = new Map<string, boolean>();
      for (const take of takes) {
        if (take.variant && !seen.has(take.variant)) {
          seen.set(take.variant, take.variant_is_common_word);
        }
      }
      return Array.from(seen, ([text, common]) => ({ text, common }));
    }, [takes]);

    useEffect(() => {
      setSelected(
        new Set(variants.filter((v) => !v.common).map((v) => v.text)),
      );
    }, [variants]);

    const reset = () => {
      setTakes([]);
      setPhase("idle");
    };

    const close = () => {
      if (phase === "recording") {
        commands.teachCancelRecording();
      }
      setOpen(false);
      reset();
    };

    const startTake = async () => {
      const result = await commands.teachStartRecording();
      if (result.status === "error") {
        toast.error(result.error);
        return;
      }
      setPhase("recording");
    };

    const stopTake = async () => {
      if (!word) return;
      setPhase("processing");
      const result = await commands.teachStopRecording(word);
      setPhase("idle");
      if (result.status === "error") {
        toast.error(result.error);
        return;
      }
      setTakes((prev) => [...prev, result.data]);
    };

    const addRule = () => {
      if (!word || selected.size === 0) return;
      const pattern = Array.from(selected).map(escapeRegex).join("|");
      const rule: TextReplacement = {
        from: `/\\b(?:${pattern})\\b/`,
        to: word,
      };
      if (!replacements.some((r) => r.from === rule.from)) {
        updateSetting("text_replacements", [...replacements, rule]);
      }
      toast.success(t("settings.vocabulary.teach.ruleAdded", { word }));
      close();
    };

    const wordOptions = customWords.map((w) => ({ value: w, label: w }));

    return (
      <>
        <SettingContainer
          title={t("settings.vocabulary.teach.title")}
          description={t("settings.vocabulary.teach.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <div className="flex items-center gap-2">
            <div className="min-w-44">
              <Select
                value={word}
                options={wordOptions}
                onChange={(value) => setWord(value)}
                placeholder={t("settings.vocabulary.teach.pickWord")}
              />
            </div>
            <Button
              onClick={() => setOpen(true)}
              disabled={!word}
              variant="primary"
              size="md"
            >
              {t("settings.vocabulary.teach.start")}
            </Button>
          </div>
        </SettingContainer>

        <Dialog
          open={open}
          onOpenChange={(next) => (next ? setOpen(true) : close())}
          title={t("settings.vocabulary.teach.dialogTitle", { word })}
          description={t("settings.vocabulary.teach.dialogDescription")}
          closeLabel={t("settings.vocabulary.teach.close")}
          closeOnBackdrop={phase === "idle"}
          footer={
            done ? (
              <div className="flex justify-end gap-2">
                <Button variant="secondary" size="md" onClick={reset}>
                  {t("settings.vocabulary.teach.again")}
                </Button>
                {variants.length > 0 ? (
                  <Button
                    variant="primary"
                    size="md"
                    onClick={addRule}
                    disabled={selected.size === 0}
                  >
                    {t("settings.vocabulary.teach.addRule")}
                  </Button>
                ) : (
                  <Button variant="primary" size="md" onClick={close}>
                    {t("settings.vocabulary.teach.done")}
                  </Button>
                )}
              </div>
            ) : undefined
          }
        >
          <div className="space-y-4">
            {!done && (
              <div className="flex items-center justify-between gap-4 rounded-lg border border-mid-gray/20 p-3">
                <div className="text-sm">
                  <div className="font-medium">
                    {t("settings.vocabulary.teach.takeLabel", {
                      n: takes.length + 1,
                      total: TAKE_MODES.length,
                    })}
                  </div>
                  <div className="text-text/70">
                    {mode === "whisper"
                      ? t("settings.vocabulary.teach.sayWhispered", { word })
                      : t("settings.vocabulary.teach.sayNormally", { word })}
                  </div>
                </div>
                {phase === "recording" ? (
                  <Button variant="primary" size="md" onClick={stopTake}>
                    {t("settings.vocabulary.teach.stop")}
                  </Button>
                ) : (
                  <Button
                    variant="primary"
                    size="md"
                    onClick={startTake}
                    disabled={phase === "processing"}
                  >
                    {phase === "processing"
                      ? t("settings.vocabulary.teach.transcribing")
                      : t("settings.vocabulary.teach.record")}
                  </Button>
                )}
              </div>
            )}

            {takes.length > 0 && (
              <ul className="space-y-1 text-sm">
                {takes.map((take, i) => (
                  <li key={i} className="flex items-center gap-2">
                    <span
                      className={
                        take.recognized ? "text-green-500" : "text-red-400"
                      }
                    >
                      {take.recognized ? "✓" : "✗"}
                    </span>
                    <span className="text-text/60 w-16 shrink-0">
                      {TAKE_MODES[i] === "whisper"
                        ? t("settings.vocabulary.teach.whisperedTag")
                        : t("settings.vocabulary.teach.spokenTag")}
                    </span>
                    <span className="truncate">
                      {take.heard || t("settings.vocabulary.teach.nothing")}
                    </span>
                  </li>
                ))}
              </ul>
            )}

            {done && variants.length === 0 && (
              <p className="text-sm text-text/80">
                {t("settings.vocabulary.teach.allRecognized", { word })}
              </p>
            )}

            {done && variants.length > 0 && (
              <div className="space-y-2">
                <p className="text-sm text-text/80">
                  {t("settings.vocabulary.teach.suggestRule", { word })}
                </p>
                {variants.map((v) => (
                  <label
                    key={v.text}
                    className="flex items-center gap-2 text-sm cursor-pointer"
                  >
                    <input
                      type="checkbox"
                      checked={selected.has(v.text)}
                      onChange={(e) => {
                        const next = new Set(selected);
                        if (e.target.checked) {
                          next.add(v.text);
                        } else {
                          next.delete(v.text);
                        }
                        setSelected(next);
                      }}
                    />
                    <span>
                      {t("settings.vocabulary.teach.variantItem", {
                        variant: v.text,
                        word,
                      })}
                    </span>
                    {v.common && (
                      <span className="text-xs text-amber-500">
                        {t("settings.vocabulary.teach.commonWord")}
                      </span>
                    )}
                  </label>
                ))}
              </div>
            )}
          </div>
        </Dialog>
      </>
    );
  },
);
