import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { convertFileSrc } from "@tauri-apps/api/core";
import { Sparkles } from "lucide-react";
import { toast } from "sonner";
import { commands, type BenchmarkItem, type BenchmarkRecord } from "@/bindings";
import { AudioPlayer, AudioPlayerGroup } from "../../ui/AudioPlayer";
import { Button } from "../../ui/Button";
import { Textarea } from "../../ui/Textarea";

const PAGE = 20;

/** Dev tool: go through benchmark recordings and write down what was said. */
export const BenchmarkReview: React.FC<{ onChange?: () => void }> = ({
  onChange,
}) => {
  const { t } = useTranslation();
  const [items, setItems] = useState<BenchmarkItem[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [guessing, setGuessing] = useState<Set<string>>(new Set());
  const [guessingAll, setGuessingAll] = useState(false);

  const load = useCallback(async (offset: number) => {
    const page = await commands.benchmarkRecords(offset, PAGE);
    setItems((prev) => (offset === 0 ? page : [...prev, ...page]));
    setHasMore(page.length === PAGE);
  }, []);

  useEffect(() => {
    load(0);
  }, [load]);

  const replace = (record: BenchmarkRecord) => {
    setItems((prev) =>
      prev.map((item) =>
        item.record.id === record.id ? { ...item, record } : item,
      ),
    );
    onChange?.();
  };

  const guess = async (id: string) => {
    setGuessing((s) => new Set(s).add(id));
    const result = await commands.guessBenchmarkGroundTruth(id);
    setGuessing((s) => {
      const next = new Set(s);
      next.delete(id);
      return next;
    });
    if (result.status === "ok") replace(result.data);
    else toast.error(result.error);
    return result.status === "ok";
  };

  const guessAllMissing = async () => {
    setGuessingAll(true);
    for (const { record } of items) {
      if (record.guess || record.ground_truth) continue;
      if (!(await guess(record.id))) break;
    }
    setGuessingAll(false);
  };

  const save = async (id: string, text: string) => {
    const result = await commands.setBenchmarkGroundTruth(id, text);
    if (result.status === "ok") replace(result.data);
    else toast.error(result.error);
  };

  if (items.length === 0) {
    return (
      <p className="px-4 py-3 text-[13px] text-text/50">
        {t("settings.app.benchmark.review.empty")}
      </p>
    );
  }

  return (
    <AudioPlayerGroup>
      <div className="flex items-center justify-between gap-3 px-4 py-3">
        <p className="text-[13px] text-text/60 max-w-md">
          {t("settings.app.benchmark.review.intro")}
        </p>
        <Button
          variant="secondary"
          size="sm"
          onClick={guessAllMissing}
          disabled={guessingAll}
        >
          <Sparkles className="w-3.5 h-3.5" />
          {guessingAll
            ? t("settings.app.benchmark.review.guessing")
            : t("settings.app.benchmark.review.guessAll")}
        </Button>
      </div>
      {items.map((item) => (
        <Entry
          key={item.record.id}
          item={item}
          guessing={guessing.has(item.record.id)}
          onGuess={() => guess(item.record.id)}
          onSave={(text) => save(item.record.id, text)}
        />
      ))}
      {hasMore && (
        <div className="px-4 py-3">
          <Button variant="ghost" size="sm" onClick={() => load(items.length)}>
            {t("settings.app.benchmark.review.more")}
          </Button>
        </div>
      )}
    </AudioPlayerGroup>
  );
};

const Field: React.FC<{ label: string; text: string | null | undefined }> = ({
  label,
  text,
}) =>
  text ? (
    <div className="grid grid-cols-[88px_1fr] gap-3 text-[13px]">
      <span className="text-text/50">{label}</span>
      <span className="text-text/85 select-text">{text}</span>
    </div>
  ) : null;

const Entry: React.FC<{
  item: BenchmarkItem;
  guessing: boolean;
  onGuess: () => void;
  onSave: (text: string) => void;
}> = ({ item: { record, audio_path }, guessing, onGuess, onSave }) => {
  const { t, i18n } = useTranslation();
  const [draft, setDraft] = useState(record.ground_truth ?? "");

  useEffect(() => {
    setDraft(record.ground_truth ?? "");
  }, [record.ground_truth]);

  const when = new Date(record.at).toLocaleString(i18n.language, {
    dateStyle: "medium",
    timeStyle: "short",
  });
  const guessText = record.guess?.text;
  const commit = (text: string) => {
    if (text.trim() !== (record.ground_truth ?? "")) onSave(text);
  };

  return (
    <div className="px-4 py-4 space-y-3">
      <div className="flex items-center justify-between gap-3 text-xs text-text/50">
        <span>
          {[when, record.app, `${record.seconds.toFixed(1)} s`]
            .filter(Boolean)
            .join(" · ")}
        </span>
        {record.ground_truth && (
          <span className="rounded-full bg-accent/10 text-accent px-2 py-0.5">
            {t("settings.app.benchmark.review.confirmed")}
          </span>
        )}
      </div>
      <AudioPlayer
        onLoadRequest={async () => convertFileSrc(audio_path, "asset")}
      />
      <div className="space-y-1.5">
        <Field
          label={t("settings.app.benchmark.review.heard")}
          text={record.transcript ?? record.error}
        />
        <Field
          label={t("settings.app.benchmark.review.pasted")}
          text={record.pasted}
        />
        {record.edit === "edited" && (
          <Field
            label={t("settings.app.benchmark.review.edited")}
            text={record.edited}
          />
        )}
      </div>

      <div className="rounded-lg bg-sunken px-3 py-2.5 space-y-1.5">
        <div className="flex items-center justify-between gap-3">
          <span className="text-[13px] font-medium text-text/70">
            {t("settings.app.benchmark.review.guess")}
            {record.guess && (
              <span className="font-normal text-text/45">
                {" "}
                · {record.guess.by}
              </span>
            )}
          </span>
          <div className="flex gap-1.5">
            {guessText !== undefined && guessText !== draft && (
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  setDraft(guessText);
                  onSave(guessText);
                }}
              >
                {t("settings.app.benchmark.review.use")}
              </Button>
            )}
            <Button
              variant="secondary"
              size="sm"
              onClick={onGuess}
              disabled={guessing}
            >
              {guessing
                ? t("settings.app.benchmark.review.guessing")
                : record.guess
                  ? t("settings.app.benchmark.review.guessAgain")
                  : t("settings.app.benchmark.review.guessOne")}
            </Button>
          </div>
        </div>
        {record.guess && (
          <>
            <p className="text-sm select-text">
              {record.guess.text || t("settings.app.benchmark.review.noSpeech")}
            </p>
            {(!record.guess.confident || record.guess.notes) && (
              <p className="text-xs text-text/55">
                {!record.guess.confident &&
                  t("settings.app.benchmark.review.unsure", {
                    words: record.guess.unsure.join(", ") || "—",
                  })}{" "}
                {record.guess.notes}
              </p>
            )}
          </>
        )}
      </div>

      <label className="block space-y-1.5">
        <span className="text-[13px] font-medium text-text/70">
          {t("settings.app.benchmark.review.truth")}
        </span>
        <Textarea
          variant="compact"
          className="w-full min-h-[60px]"
          value={draft}
          placeholder={t("settings.app.benchmark.review.truthPlaceholder")}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={() => commit(draft)}
        />
      </label>
    </div>
  );
};
