/* Pieces of a meeting in progress, shared by the Meetings page and the
 * side panel that shows while recording. */
import React, {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
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
import {
  afterInput,
  onEnter,
  onTab,
  toggleBullet,
  type Edit,
} from "@/lib/utils/bullets";

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

export type NotesEditorHandle = {
  toggleBullet: () => void;
  focusEnd: () => void;
};

/** The user's own notes, saved as they type. They go into the summary.
 * "- " starts a bullet list. `bare`: no heading or box, for the panel,
 * which has its own. */
export const NotesEditor = forwardRef<
  NotesEditorHandle,
  { id: string; minRows?: number; bare?: boolean }
>(function NotesEditor({ id, minRows = 5, bare = false }, ref) {
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

  const change = (text: string) => {
    setNotes(text);
    pending.current = text;
    setSaveState("idle");
    clearTimeout(timer.current);
    timer.current = setTimeout(flush, 600);
  };

  // Where the cursor goes after a list edit, once the text is in.
  const cursorAfter = useRef<number | null>(null);
  useLayoutEffect(() => {
    const el = area.current;
    if (el && cursorAfter.current !== null) {
      el.setSelectionRange(cursorAfter.current, cursorAfter.current);
      cursorAfter.current = null;
    }
  }, [notes]);
  const apply = (edit: Edit | null) => {
    if (!edit) return false;
    cursorAfter.current = edit.cursor;
    change(edit.text);
    return true;
  };

  useImperativeHandle(ref, () => ({
    toggleBullet: () => {
      const el = area.current;
      if (!el) return;
      el.focus();
      apply(toggleBullet(el.value, el.selectionStart));
    },
    focusEnd: () => {
      const el = area.current;
      if (!el) return;
      el.focus();
      el.setSelectionRange(el.value.length, el.value.length);
    },
  }));

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    const el = e.currentTarget;
    if (el.selectionStart !== el.selectionEnd || e.nativeEvent.isComposing) {
      return;
    }
    const edit =
      e.key === "Enter" && !e.shiftKey
        ? onEnter(el.value, el.selectionStart)
        : e.key === "Tab"
          ? onTab(el.value, el.selectionStart, e.shiftKey)
          : null;
    if (apply(edit)) e.preventDefault();
  };

  if (notes === null) return null;
  const textarea = (
    <textarea
      ref={area}
      value={notes}
      rows={minRows}
      onChange={(e) => {
        const el = e.target;
        if (!apply(afterInput(el.value, el.selectionStart))) {
          change(el.value);
        }
      }}
      onKeyDown={onKeyDown}
      onBlur={flush}
      placeholder={t("meetings.notes.placeholder")}
      className={
        bare
          ? "w-full resize-none bg-transparent text-sm leading-relaxed outline-none placeholder:text-text/35"
          : "w-full resize-none rounded-xl border border-stone/20 bg-background px-4 py-3 text-sm leading-relaxed outline-none focus:border-accent/60 placeholder:text-text/35"
      }
    />
  );
  if (bare) return textarea;
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
      {textarea}
    </div>
  );
});

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
