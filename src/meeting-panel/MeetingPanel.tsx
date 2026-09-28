import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import { ChevronLeft, ExternalLink, Minus, Square } from "lucide-react";
import {
  commands,
  type MeetingPanelState,
  type MeetingState,
} from "@/bindings";
import {
  clock,
  LiveHelp,
  NotesEditor,
  TitleEditor,
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

/** The side panel while a meeting records: timer, Stop, and the notes
 *  scratchpad; collapses to a pill. */
const MeetingPanel: React.FC = () => {
  const { t } = useTranslation();
  const [live, setLive] = useState<MeetingState | null>(null);
  const [receivedAt, setReceivedAt] = useState(Date.now());
  const [now, setNow] = useState(Date.now());
  const [expanded, setExpanded] = useState(true);
  const [stopping, setStopping] = useState(false);

  useEffect(() => {
    const take = (state: MeetingState) => {
      setLive(state);
      setReceivedAt(Date.now());
      if (state.recording) setStopping(false);
    };
    commands.getMeetingState().then(take);
    commands.getMeetingPanelState().then((s) => setExpanded(s.expanded));
    const unlisten = [
      listen<MeetingState>("meeting-state", (e) => take(e.payload)),
      listen<MeetingPanelState>("meeting-panel", (e) =>
        setExpanded(e.payload.expanded),
      ),
    ];
    const timer = setInterval(() => setNow(Date.now()), 500);
    return () => {
      clearInterval(timer);
      unlisten.forEach((p) => p.then((f) => f()));
    };
  }, []);

  const recording = live?.recording;
  if (!live || !recording) return null;
  const elapsed = clock(live.elapsed_ms + (now - receivedAt));

  const stop = async () => {
    setStopping(true);
    const result = await commands.stopMeeting();
    if (result.status === "error") {
      setStopping(false);
      toast.error(t("meetings.errors.stop", { error: result.error }));
    }
  };

  const expand = (on: boolean) => {
    setExpanded(on);
    commands.setMeetingPanelExpanded(on);
  };

  const stopButton = (
    <button
      onClick={stop}
      disabled={stopping}
      title={t("meetings.stop")}
      className="shrink-0 inline-flex items-center gap-1.5 rounded-full bg-error px-3 py-1 text-sm text-white hover:opacity-90 cursor-pointer disabled:opacity-50"
    >
      <Square className="w-3 h-3 fill-current" />
      {expanded && t("meetings.stop")}
    </button>
  );

  if (!expanded) {
    // A slim tab on the screen's right edge: drag it up and down; it can't
    // be closed, so the notes are always a click away.
    return (
      <div className="h-full w-full py-1 ps-1">
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
            <Waves />
            <span className="text-[10px] tabular-nums text-text/70">
              {elapsed}
            </span>
          </button>
          <button
            onClick={stop}
            disabled={stopping}
            title={t("meetings.stop")}
            className="rounded-full bg-error p-2 text-white hover:opacity-90 cursor-pointer disabled:opacity-50"
          >
            <Square className="w-3 h-3 fill-current" />
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="h-full w-full p-1.5">
      <div className="h-full flex flex-col overflow-hidden rounded-2xl border border-stone/20 bg-background/95 shadow-lg backdrop-blur">
        <div
          data-tauri-drag-region
          className="flex items-center gap-2 px-3 pt-2.5 pb-1"
        >
          <Waves />
          <span
            data-tauri-drag-region
            className="flex-1 text-sm tabular-nums text-text/70"
          >
            {t("meetings.recordingFor", { time: elapsed })}
          </span>
          <button
            onClick={() => expand(false)}
            title={t("meetings.panel.collapse")}
            className={iconButton}
          >
            <Minus className="w-3.5 h-3.5" />
          </button>
        </div>
        <div className="flex-1 overflow-y-auto px-3 pb-3 space-y-3">
          <TitleEditor
            key={`${recording.id}-${recording.title}`}
            meeting={recording}
            className="text-xl"
          />
          <NotesEditor id={recording.id} minRows={8} />
          {live.live && <LiveHelp />}
        </div>
        <div className="flex items-center gap-2 border-t border-stone/15 px-3 py-2">
          {stopButton}
          <span className="flex-1" />
          <button
            onClick={() => commands.openMeetingsPage()}
            className="inline-flex items-center gap-1 text-xs text-text/55 hover:text-text cursor-pointer"
          >
            {t("meetings.panel.openInFelix")}
            <ExternalLink className="w-3 h-3" />
          </button>
        </div>
      </div>
    </div>
  );
};

export default MeetingPanel;
