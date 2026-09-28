/* Pieces of a meeting in progress, shared by the Meetings page and the
 * side panel that shows while recording. */
import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import { Loader2, Sparkles } from "lucide-react";
import {
  commands,
  type LiveQuestion,
  type MeetingInfo,
  type MeetingNotes,
} from "@/bindings";

/** `m:ss`, or `h:mm:ss` from an hour. */
export const clock = (ms: number) => {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
};

/** Animated level bars for the recording pill. */
export const LiveBars: React.FC = () => (
  <span className="flex items-end gap-[3px] h-4" aria-hidden>
    {[0, 1, 2, 3].map((i) => (
      <span
        key={i}
        className="meeting-bar w-[3px] rounded-full bg-current"
        style={{ animationDelay: `${i * 0.15}s` }}
      />
    ))}
  </span>
);

export const useMeetingTitle = () => {
  const { t } = useTranslation();
  return (m: MeetingInfo) =>
    m.title ??
    (m.mode === "call"
      ? t("meetings.title.call")
      : t("meetings.title.inPerson"));
};

/** The meeting's title, edited in place like a document heading. Keyed by
 * id and title, so it resets when either changes. */
export const TitleEditor: React.FC<{
  meeting: MeetingInfo;
  className?: string;
}> = ({ meeting, className = "" }) => {
  const { t } = useTranslation();
  const titleOf = useMeetingTitle();
  const [value, setValue] = useState(titleOf(meeting));

  const save = async () => {
    if (value.trim() === titleOf(meeting)) return;
    const result = await commands.renameMeeting(meeting.id, value);
    if (result.status === "error") toast.error(result.error);
  };
  return (
    <input
      value={value}
      onChange={(e) => setValue(e.target.value)}
      onBlur={save}
      onKeyDown={(e) => {
        if (e.key === "Enter") e.currentTarget.blur();
      }}
      placeholder={t("meetings.titlePlaceholder")}
      className={`w-full bg-transparent font-serif outline-none placeholder:text-text/30 ${className}`}
    />
  );
};

type SaveState = "idle" | "saving" | "saved";

/** The user's own notes, saved as they type. They go into the summary. */
export const NotesEditor: React.FC<{ id: string; minRows?: number }> = ({
  id,
  minRows = 5,
}) => {
  const { t } = useTranslation();
  const [notes, setNotes] = useState<string | null>(null);
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const pending = useRef<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>();
  const area = useRef<HTMLTextAreaElement>(null);

  const flush = useCallback(async () => {
    clearTimeout(timer.current);
    const text = pending.current;
    if (text === null) return;
    pending.current = null;
    setSaveState("saving");
    const result = await commands.saveMeetingNotes(id, text);
    if (result.status === "error") {
      toast.error(t("meetings.notes.saveFailed", { error: result.error }));
      setSaveState("idle");
    } else {
      setSaveState("saved");
    }
  }, [id, t]);

  // Saved from the other window (the side panel or the Meetings page):
  // take them unless something typed here is still to be saved.
  useEffect(() => {
    const unlisten = listen<MeetingNotes>("meeting-notes-changed", (e) => {
      if (e.payload.id !== id || pending.current !== null) return;
      if (document.activeElement === area.current) return;
      setNotes(e.payload.notes);
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, [id]);

  useEffect(() => {
    let cancelled = false;
    commands.getMeetingNotes(id).then((result) => {
      if (!cancelled) setNotes(result.status === "ok" ? result.data : "");
    });
    return () => {
      cancelled = true;
      flush();
    };
  }, [id, flush]);

  // Grow with the text.
  useEffect(() => {
    const el = area.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [notes]);

  if (notes === null) return null;
  return (
    <div className="space-y-1">
      <div className="flex items-center justify-between px-1">
        <span className="text-sm font-medium text-text/70">
          {t("meetings.notes.title")}
        </span>
        <span className="text-xs text-text/40">
          {saveState === "saving"
            ? t("meetings.notes.saving")
            : saveState === "saved"
              ? t("meetings.notes.saved")
              : ""}
        </span>
      </div>
      <textarea
        ref={area}
        value={notes}
        rows={minRows}
        onChange={(e) => {
          setNotes(e.target.value);
          pending.current = e.target.value;
          setSaveState("idle");
          clearTimeout(timer.current);
          timer.current = setTimeout(flush, 600);
        }}
        onBlur={flush}
        placeholder={t("meetings.notes.placeholder")}
        className="w-full resize-none rounded-xl border border-stone/20 bg-background px-4 py-3 text-sm leading-relaxed outline-none focus:border-accent/60 placeholder:text-text/35"
      />
    </div>
  );
};

/** "What did I miss?" and "Suggest a question" while a call records. */
export const LiveHelp: React.FC = () => {
  const { t } = useTranslation();
  const [answer, setAnswer] = useState<string | null>(null);
  const [asking, setAsking] = useState<LiveQuestion | null>(null);

  const ask = async (question: LiveQuestion) => {
    setAsking(question);
    const result = await commands.askLive(question);
    setAsking(null);
    if (result.status === "error") toast.error(result.error);
    else setAnswer(result.data || t("meetings.live.nothingYet"));
  };

  const button =
    "inline-flex items-center gap-1.5 rounded-full border border-stone/25 px-3 py-1 text-sm hover:border-accent/50 cursor-pointer disabled:opacity-50";
  return (
    <div className="space-y-2">
      <div className="flex flex-wrap gap-2">
        {(["missed", "question"] as const).map((q) => (
          <button
            key={q}
            onClick={() => ask(q)}
            disabled={asking !== null}
            className={button}
          >
            {asking === q ? (
              <Loader2 className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <Sparkles className="w-3.5 h-3.5" />
            )}
            {t(`meetings.live.${q}`)}
          </button>
        ))}
      </div>
      {answer && (
        <div className="rounded-lg bg-accent/5 px-3 py-2 text-sm leading-relaxed">
          <p className="whitespace-pre-wrap">{answer}</p>
          <button
            onClick={() => setAnswer(null)}
            className="mt-1 text-xs text-text/50 hover:text-text cursor-pointer"
          >
            {t("meetings.live.dismiss")}
          </button>
        </div>
      )}
    </div>
  );
};
