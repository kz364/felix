import React, { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import {
  ChevronLeft,
  ExternalLink,
  List,
  Minus,
  Pause,
  Play,
  Square,
} from "lucide-react";
import {
  commands,
  type MeetingInfo,
  type MeetingPanelState,
  type MeetingState,
  type LiveParagraph,
} from "@/bindings";
import {
  clock,
  LiveHelp,
  NotesEditor,
  TitleEditor,
  type NotesEditorHandle,
} from "@/components/meetings/live";

/** Bars in the tab's waves, and how often they follow the sound. */
const WAVE_BARS = 5;
const WAVE_EVERY_MS = 90;
/** Each bar's share of the level, so the waves have a shape. */
const WAVE_SHAPE = [0.55, 0.85, 1, 0.8, 0.6];

/** Sound waves that follow how loud the meeting is right now, so it's plain
 *  that it's recording (and hearing something). */
const Waves: React.FC = () => {
  const [levels, setLevels] = useState<number[]>(() =>
    Array(WAVE_BARS).fill(0),
  );
  const tick = useRef(0);
  useEffect(() => {
    let alive = true;
    const timer = setInterval(async () => {
      const level = await commands.meetingLevel().catch(() => 0);
      if (!alive) return;
      tick.current += 1;
      setLevels(
        WAVE_SHAPE.map((shape, i) => {
          // A little movement even when it's quiet, so it looks alive.
          const idle = 0.12 + 0.06 * Math.sin(tick.current * 0.7 + i * 1.3);
          const jitter = 0.8 + 0.2 * Math.sin(tick.current * 1.9 + i * 2.1);
          return Math.max(idle, level * shape * jitter);
        }),
      );
    }, WAVE_EVERY_MS);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, []);
  return (
    <span className="flex items-center gap-[3px] h-7" aria-hidden>
      {levels.map((l, i) => (
        <span
          key={i}
          className="w-[3px] rounded-full bg-error transition-[height] duration-100 ease-out"
          style={{ height: `${Math.round(4 + l * 24)}px` }}
        />
      ))}
    </span>
  );
};

const iconButton =
  "shrink-0 rounded-full p-1.5 text-text/50 hover:text-text hover:bg-stone/10 cursor-pointer";

/** How long the panel takes to fold away before the window shrinks
 *  (matches `.panel-close` in App.css). */
const CLOSE_MS = 170;

type Tab = "notes" | "transcript";

/** The side panel while a meeting records: timer, Pause and Stop, and the
 *  notes and running transcript under a fixed row of tabs; folds into a
 *  tab on the screen's edge. */
const MeetingPanel: React.FC = () => {
  const { t } = useTranslation();
  const [live, setLive] = useState<MeetingState | null>(null);
  const [receivedAt, setReceivedAt] = useState(Date.now());
  const [now, setNow] = useState(Date.now());
  const [expanded, setExpanded] = useState(true);
  const [closing, setClosing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [tab, setTab] = useState<Tab>("notes");
  const notes = useRef<NotesEditorHandle>(null);
  const scroller = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const take = (state: MeetingState) => {
      setLive(state);
      setReceivedAt(Date.now());
      setBusy(false);
    };
    commands.getMeetingState().then(take);
    commands.getMeetingPanelState().then((s) => setExpanded(s.expanded));
    const unlisten = [
      listen<MeetingState>("meeting-state", (e) => take(e.payload)),
      listen<MeetingPanelState>("meeting-panel", (e) => {
        setClosing(false);
        setExpanded(e.payload.expanded);
      }),
    ];
    const timer = setInterval(() => setNow(Date.now()), 500);
    return () => {
      clearInterval(timer);
      unlisten.forEach((p) => p.then((f) => f()));
    };
  }, []);

  const meeting = live?.recording ?? live?.paused;
  if (!live || !meeting) return null;
  const paused = !live.recording;
  const elapsed = clock(live.elapsed_ms + (paused ? 0 : now - receivedAt));

  const run = async (
    action: () => Promise<
      { status: "ok" } | { status: "error"; error: string }
    >,
    error: string,
  ) => {
    setBusy(true);
    const result = await action();
    if (result.status === "error") {
      setBusy(false);
      toast.error(t(error, { error: result.error }));
    }
  };
  const stop = () => run(commands.stopMeeting, "meetings.errors.stop");
  const pause = () => run(commands.pauseMeeting, "meetings.errors.pause");
  const resume = () =>
    run(commands.resumePausedMeeting, "meetings.errors.resume");

  const expand = (on: boolean) => {
    if (on) {
      // The window grows first; the panel animates in when it hears back.
      commands.setMeetingPanelExpanded(true);
      return;
    }
    setClosing(true);
    setTimeout(() => commands.setMeetingPanelExpanded(false), CLOSE_MS);
  };

  const status = (
    <span className="flex items-center gap-1 text-[10px] tabular-nums text-text/70">
      {elapsed}
    </span>
  );

  if (!expanded) {
    // A slim tab on the screen's right edge: drag it up and down; it can't
    // be closed, so the notes are always a click away. Fixed size in the
    // top right, so it doesn't stretch while the window grows.
    return (
      <div className="tab-in absolute top-0 end-0 h-[176px] w-[48px] py-1 ps-1">
        <div
          data-tauri-drag-region
          className="h-full flex flex-col items-center justify-between gap-1 rounded-s-2xl border border-e-0 border-stone/20 bg-background/95 py-2 shadow-md backdrop-blur"
        >
          <button
            onClick={() => expand(true)}
            title={t("meetings.panel.expand")}
            className={iconButton}
          >
            <ChevronLeft className="w-4 h-4" />
          </button>
          <button
            onClick={() => expand(true)}
            title={t("meetings.panel.expand")}
            className="flex flex-col items-center gap-1 cursor-pointer"
          >
            {paused ? (
              <Pause className="h-7 w-4 text-text/50 fill-current" />
            ) : (
              <Waves />
            )}
            {status}
          </button>
          {paused ? (
            <button
              onClick={resume}
              disabled={busy}
              title={t("meetings.panel.resume")}
              className="rounded-full bg-accent p-2 text-white hover:opacity-90 cursor-pointer disabled:opacity-50"
            >
              <Play className="w-3 h-3 fill-current" />
            </button>
          ) : (
            <button
              onClick={stop}
              disabled={busy}
              title={t("meetings.stop")}
              className="rounded-full bg-error p-2 text-white hover:opacity-90 cursor-pointer disabled:opacity-50"
            >
              <Square className="w-3 h-3 fill-current" />
            </button>
          )}
        </div>
      </div>
    );
  }

  const tabButton = (id: Tab) => (
    <button
      key={id}
      role="tab"
      aria-selected={tab === id}
      onClick={() => setTab(id)}
      className={`-mb-px border-b-2 px-1 pb-1.5 text-sm cursor-pointer ${
        tab === id
          ? "border-accent text-text"
          : "border-transparent text-text/50 hover:text-text"
      }`}
    >
      {t(`meetings.panel.tabs.${id}`)}
    </button>
  );

  return (
    <div className="h-full w-full p-1.5">
      <div
        className={`${closing ? "panel-close" : "panel-open"} h-full flex flex-col overflow-hidden rounded-2xl border border-stone/20 bg-background/95 shadow-lg backdrop-blur`}
      >
        <div
          data-tauri-drag-region
          className="flex items-center gap-2 px-3 pt-2.5 pb-1"
        >
          {paused ? (
            <Pause className="h-7 w-4 text-text/50 fill-current" />
          ) : (
            <Waves />
          )}
          <span
            data-tauri-drag-region
            className="flex-1 text-sm tabular-nums text-text/70"
          >
            {paused
              ? t("meetings.panel.pausedAt", { time: elapsed })
              : t("meetings.recordingFor", { time: elapsed })}
          </span>
          <button
            onClick={() => expand(false)}
            title={t("meetings.panel.collapse")}
            className={iconButton}
          >
            <Minus className="w-3.5 h-3.5" />
          </button>
        </div>
        <div className="px-3">
          <TitleEditor
            key={`${meeting.id}-${meeting.title}`}
            meeting={meeting}
            className="text-xl"
          />
        </div>
        <div
          role="tablist"
          className="mt-1 flex items-end gap-4 border-b border-stone/15 px-3"
        >
          {tabButton("notes")}
          {tabButton("transcript")}
          <span className="flex-1" />
          {tab === "notes" && (
            <button
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => notes.current?.toggleBullet()}
              title={t("meetings.panel.bullet")}
              className={`${iconButton} mb-1`}
            >
              <List className="w-3.5 h-3.5" />
            </button>
          )}
        </div>
        <div
          ref={scroller}
          onClick={(e) => {
            // Below the notes: carry on writing at the end.
            if (tab === "notes" && e.target === e.currentTarget) {
              notes.current?.focusEnd();
            }
          }}
          className="flex-1 overflow-y-auto px-3 py-2.5"
        >
          {tab === "notes" ? (
            <NotesEditor ref={notes} id={meeting.id} minRows={14} bare={true} />
          ) : (
            <LiveTranscript
              meeting={meeting}
              scroller={scroller}
              help={live.live && !paused}
            />
          )}
        </div>
        <div className="flex items-center gap-2 border-t border-stone/15 px-3 py-2">
          {paused ? (
            <button
              onClick={resume}
              disabled={busy}
              className="shrink-0 inline-flex items-center gap-1.5 rounded-full bg-accent px-3 py-1 text-sm text-white hover:opacity-90 cursor-pointer disabled:opacity-50"
            >
              <Play className="w-3 h-3 fill-current" />
              {t("meetings.panel.resume")}
            </button>
          ) : (
            <button
              onClick={pause}
              disabled={busy}
              className="shrink-0 inline-flex items-center gap-1.5 rounded-full border border-stone/25 px-3 py-1 text-sm hover:border-stone/50 cursor-pointer disabled:opacity-50"
            >
              <Pause className="w-3 h-3 fill-current" />
              {t("meetings.panel.pause")}
            </button>
          )}
          <button
            onClick={stop}
            disabled={busy}
            className="shrink-0 inline-flex items-center gap-1.5 rounded-full bg-error px-3 py-1 text-sm text-white hover:opacity-90 cursor-pointer disabled:opacity-50"
          >
            <Square className="w-3 h-3 fill-current" />
            {t("meetings.stop")}
          </button>
          <span className="flex-1" />
          <button
            onClick={() => commands.openMeetingsPage()}
            title={t("meetings.panel.openInFelix")}
            className={iconButton}
          >
            <ExternalLink className="w-3.5 h-3.5" />
          </button>
        </div>
      </div>
    </div>
  );
};

/** How close to the bottom counts as reading the latest (px). */
const FOLLOW_SLACK = 48;
/** Checked this often too, in case an update was missed. */
const TRANSCRIPT_POLL_MS = 10_000;

/** The rough transcript so far, newest at the bottom, following along
 *  unless the user has scrolled up to read. */
const LiveTranscript: React.FC<{
  meeting: MeetingInfo;
  scroller: React.RefObject<HTMLDivElement | null>;
  help: boolean;
}> = ({ meeting, scroller, help }) => {
  const { t } = useTranslation();
  const [paragraphs, setParagraphs] = useState<LiveParagraph[] | null>(null);
  const follow = useRef(true);

  useEffect(() => {
    let alive = true;
    const load = () =>
      commands.getLiveTranscript(meeting.id).then((r) => {
        if (alive && r.status === "ok") setParagraphs(r.data);
      });
    load();
    const unlisten = listen<string>("meeting-live-transcript", (e) => {
      if (e.payload === meeting.id) load();
    });
    const timer = setInterval(load, TRANSCRIPT_POLL_MS);
    return () => {
      alive = false;
      clearInterval(timer);
      unlisten.then((f) => f());
    };
  }, [meeting.id]);

  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const onScroll = () => {
      follow.current =
        el.scrollHeight - el.scrollTop - el.clientHeight < FOLLOW_SLACK;
    };
    el.addEventListener("scroll", onScroll);
    return () => el.removeEventListener("scroll", onScroll);
  }, [scroller]);

  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && follow.current) el.scrollTop = el.scrollHeight;
  }, [paragraphs, scroller]);

  const who = (p: LiveParagraph) =>
    meeting.mode !== "call"
      ? null
      : p.source === "mic"
        ? t("meetings.speaker.me")
        : (p.name ?? t("meetings.speaker.them"));

  return (
    <div className="space-y-3">
      {help && <LiveHelp />}
      {paragraphs === null ? null : paragraphs.length === 0 ? (
        <p className="text-sm text-text/45">
          {t("meetings.panel.transcriptEmpty")}
        </p>
      ) : (
        paragraphs.map((p) => (
          <div key={`${p.source}-${p.start_ms}`} className="text-sm">
            <div className="flex items-baseline gap-2 text-xs text-text/45">
              {who(p) && <span className="font-medium">{who(p)}</span>}
              <span className="tabular-nums">{clock(p.start_ms)}</span>
            </div>
            <p className="leading-relaxed">{p.text}</p>
          </div>
        ))
      )}
    </div>
  );
};

export default MeetingPanel;
