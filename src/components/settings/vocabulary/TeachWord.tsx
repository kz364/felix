import React, { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Check, X } from "lucide-react";
import { commands, type TeachTake } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { Button } from "../../ui/Button";
import { Input } from "../../ui/Input";

// Three normal takes, then two whispered: the pipeline has to cope with both.
const TAKE_MODES = [
  "normal",
  "normal",
  "normal",
  "whisper",
  "whisper",
] as const;

/** Add a word to the vocabulary, then say it a few times so Felix learns
 *  how the current model mishears it. */
export const TeachWord: React.FC<{ autoFocus?: boolean }> = ({
  autoFocus = false,
}) => {
  const { t } = useTranslation();
  const { refreshSettings } = useSettings();
  const [draft, setDraft] = useState("");
  const [word, setWord] = useState<string | null>(null);
  const [phase, setPhase] = useState<"idle" | "recording" | "processing">(
    "idle",
  );
  const [takes, setTakes] = useState<TeachTake[]>([]);
  const [saved, setSaved] = useState(false);

  const done = takes.length >= TAKE_MODES.length;
  const mode = TAKE_MODES[Math.min(takes.length, TAKE_MODES.length - 1)];

  // Unique mishearings, except common words (they'd rewrite normal speech).
  const variants = useMemo(() => {
    const seen = new Map<string, boolean>();
    for (const take of takes) {
      if (take.variant && !seen.has(take.variant)) {
        seen.set(take.variant, take.variant_is_common_word);
      }
    }
    return Array.from(seen, ([text, common]) => ({ text, common }));
  }, [takes]);

  // Leaving the step mid-way: stop recording, drop unsaved clips.
  const latest = useRef({ phase, takes, saved });
  latest.current = { phase, takes, saved };
  useEffect(() => {
    return () => {
      const { phase, takes, saved } = latest.current;
      if (phase === "recording") commands.teachCancelRecording();
      if (!saved && takes.length > 0) {
        commands.teachDiscardClips(takes.map((take) => take.clip.file));
      }
    };
  }, []);

  const begin = async () => {
    const w = draft.trim();
    if (!w) return;
    const result = await commands.addVocabularyWord(w);
    if (result.status === "error") {
      toast.error(result.error);
      return;
    }
    setWord(w);
  };

  const record = async () => {
    const result = await commands.teachStartRecording();
    if (result.status === "error") {
      toast.error(result.error);
      return;
    }
    setPhase("recording");
  };

  const stop = async () => {
    if (!word) return;
    setPhase("processing");
    const result = await commands.teachStopRecording(word, mode === "whisper");
    setPhase("idle");
    if (result.status === "error") {
      toast.error(result.error);
      return;
    }
    setTakes((prev) => [...prev, result.data]);
  };

  const save = async () => {
    if (!word) return;
    const all = variants.map((v) => v.text);
    const result = await commands.teachSaveWord(
      word,
      takes.map((take) => take.clip),
      takes.map((take) => take.heard),
      all,
      variants.filter((v) => v.common).map((v) => v.text),
    );
    if (result.status === "error") {
      toast.error(result.error);
      return;
    }
    await refreshSettings();
    setSaved(true);
  };

  const restart = () => {
    const files = takes.map((take) => take.clip.file);
    if (files.length > 0) commands.teachDiscardClips(files);
    setTakes([]);
    setSaved(false);
  };

  // Back to the word box; a saved word keeps its clips.
  const another = () => {
    if (!saved) restart();
    setTakes([]);
    setSaved(false);
    setDraft("");
    setWord(null);
  };

  if (!word) {
    return (
      <div className="flex gap-2">
        <Input
          autoFocus={autoFocus}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && begin()}
          placeholder={t("onboarding.guide.teach.placeholder")}
          className="flex-1"
        />
        <Button onClick={begin} disabled={!draft.trim()}>
          {t("onboarding.guide.teach.start")}
        </Button>
      </div>
    );
  }

  return (
    <div className="space-y-3">
      {!done && (
        <div className="rounded-xl border border-stone/20 bg-surface">
          <div className="flex items-center justify-between gap-4 p-3">
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
              <Button onClick={stop}>
                {t("settings.vocabulary.teach.stop")}
              </Button>
            ) : (
              <Button onClick={record} disabled={phase === "processing"}>
                {phase === "processing"
                  ? t("settings.vocabulary.teach.transcribing")
                  : t("settings.vocabulary.teach.record")}
              </Button>
            )}
          </div>
        </div>
      )}

      {!done && (
        <button
          type="button"
          onClick={another}
          className="text-xs text-text/60 hover:text-text cursor-pointer"
        >
          {t("settings.vocabulary.teach.differentWord")}
        </button>
      )}

      {takes.length > 0 && (
        <ul className="space-y-1 text-sm">
          {takes.map((take, i) => (
            <li key={take.clip.file} className="flex items-center gap-2">
              {take.recognized ? (
                <Check className="h-4 w-4 text-success" />
              ) : (
                <X className="h-4 w-4 text-error" />
              )}
              <span className="text-text/60 w-20 shrink-0">
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

      {done && (
        <div className="space-y-3">
          <p className="text-sm text-text/80">
            {variants.length === 0
              ? t("settings.vocabulary.teach.allRecognized", { word })
              : t("onboarding.guide.teach.willRewrite", {
                  word,
                  variants: variants
                    .filter((v) => !v.common)
                    .map((v) => `“${v.text}”`)
                    .join(", "),
                })}
          </p>
          {saved ? (
            <div className="flex items-center justify-between gap-2">
              <p className="flex items-center gap-1.5 text-sm text-success">
                <Check className="h-4 w-4" />
                {t("settings.vocabulary.teach.saved", { word })}
              </p>
              <Button variant="secondary" size="sm" onClick={another}>
                {t("settings.vocabulary.teach.another")}
              </Button>
            </div>
          ) : (
            <div className="flex gap-2">
              <Button variant="secondary" onClick={restart}>
                {t("settings.vocabulary.teach.again")}
              </Button>
              <Button onClick={save}>
                {t("settings.vocabulary.teach.done")}
              </Button>
            </div>
          )}
        </div>
      )}
    </div>
  );
};
