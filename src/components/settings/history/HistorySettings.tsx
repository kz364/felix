import React, { useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { readFile } from "@tauri-apps/plugin-fs";
import { Check, Copy, FolderOpen, RotateCcw, Star, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  commands,
  events,
  type HistoryEntry,
  type HistoryUpdatePayload,
} from "@/bindings";
import { useOsType } from "@/hooks/useOsType";
import { formatDateTime } from "@/utils/dateFormat";
import { AudioPlayer, AudioPlayerGroup } from "../../ui/AudioPlayer";
import { Button } from "../../ui/Button";
import { PageHeader } from "../../ui/PageHeader";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { HistoryRetention } from "../HistoryRetention";
import { copyToClipboard } from "./clipboard";

const IconButton: React.FC<{
  onClick: () => void;
  title: string;
  disabled?: boolean;
  active?: boolean;
  children: React.ReactNode;
}> = ({ onClick, title, disabled, active, children }) => (
  <button
    onClick={onClick}
    disabled={disabled}
    className={`p-1.5 rounded-md flex items-center justify-center transition-colors cursor-pointer disabled:cursor-not-allowed disabled:text-text/20 ${
      active
        ? "text-accent hover:text-accent/80"
        : "text-text/45 hover:text-text hover:bg-stone/10"
    }`}
    title={title}
  >
    {children}
  </button>
);

const PAGE_SIZE = 30;

interface OpenRecordingsButtonProps {
  onClick: () => void;
  label: string;
}

const OpenRecordingsButton: React.FC<OpenRecordingsButtonProps> = ({
  onClick,
  label,
}) => (
  <Button onClick={onClick} variant="secondary" size="sm" title={label}>
    <FolderOpen className="w-3.5 h-3.5" />
    <span>{label}</span>
  </Button>
);

export const HistorySettings: React.FC = () => {
  const { t } = useTranslation();
  const osType = useOsType();
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [hasMore, setHasMore] = useState(true);
  const sentinelRef = useRef<HTMLDivElement>(null);
  const entriesRef = useRef<HistoryEntry[]>([]);
  const loadingRef = useRef(false);

  // Keep ref in sync for use in IntersectionObserver callback
  useEffect(() => {
    entriesRef.current = entries;
  }, [entries]);

  const loadPage = useCallback(async (cursor?: number) => {
    const isFirstPage = cursor === undefined;
    if (!isFirstPage && loadingRef.current) return;
    loadingRef.current = true;

    if (isFirstPage) setLoading(true);

    try {
      const result = await commands.getHistoryEntries(
        cursor ?? null,
        PAGE_SIZE,
      );
      if (result.status === "ok") {
        const { entries: newEntries, has_more } = result.data;
        setEntries((prev) =>
          isFirstPage ? newEntries : [...prev, ...newEntries],
        );
        setHasMore(has_more);
      }
    } catch (error) {
      console.error("Failed to load history entries:", error);
    } finally {
      setLoading(false);
      loadingRef.current = false;
    }
  }, []);

  // Initial load
  useEffect(() => {
    loadPage();
  }, [loadPage]);

  // Infinite scroll via IntersectionObserver
  useEffect(() => {
    if (loading) return;

    const sentinel = sentinelRef.current;
    if (!sentinel || !hasMore) return;

    const observer = new IntersectionObserver(
      (observerEntries) => {
        const first = observerEntries[0];
        if (first.isIntersecting) {
          const lastEntry = entriesRef.current[entriesRef.current.length - 1];
          if (lastEntry) {
            loadPage(lastEntry.id);
          }
        }
      },
      { threshold: 0 },
    );

    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [loading, hasMore, loadPage]);

  // Listen for new entries added from the transcription pipeline
  useEffect(() => {
    const unlisten = events.historyUpdatePayload.listen((event) => {
      const payload: HistoryUpdatePayload = event.payload;
      if (payload.action === "added") {
        setEntries((prev) => [payload.entry, ...prev]);
      } else if (payload.action === "updated") {
        setEntries((prev) =>
          prev.map((e) => (e.id === payload.entry.id ? payload.entry : e)),
        );
      }
      // "deleted" and "toggled" are handled by optimistic updates only,
      // so we intentionally ignore them here to avoid double-mutation.
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const toggleSaved = async (id: number) => {
    // Optimistic update
    setEntries((prev) =>
      prev.map((e) => (e.id === id ? { ...e, saved: !e.saved } : e)),
    );
    try {
      const result = await commands.toggleHistoryEntrySaved(id);
      if (result.status !== "ok") {
        // Revert on failure
        setEntries((prev) =>
          prev.map((e) => (e.id === id ? { ...e, saved: !e.saved } : e)),
        );
      }
    } catch (error) {
      console.error("Failed to toggle saved status:", error);
      // Revert on failure
      setEntries((prev) =>
        prev.map((e) => (e.id === id ? { ...e, saved: !e.saved } : e)),
      );
    }
  };

  const getAudioUrl = useCallback(
    async (fileName: string) => {
      try {
        const result = await commands.getAudioFilePath(fileName);
        if (result.status === "ok") {
          if (osType === "linux") {
            const fileData = await readFile(result.data);
            const blob = new Blob([fileData], { type: "audio/wav" });
            return URL.createObjectURL(blob);
          }
          return convertFileSrc(result.data, "asset");
        }
        return null;
      } catch (error) {
        console.error("Failed to get audio file path:", error);
        return null;
      }
    },
    [osType],
  );

  const deleteAudioEntry = async (id: number) => {
    // Optimistically remove
    setEntries((prev) => prev.filter((e) => e.id !== id));
    try {
      const result = await commands.deleteHistoryEntry(id);
      if (result.status !== "ok") {
        // Reload on failure
        loadPage();
      }
    } catch (error) {
      console.error("Failed to delete entry:", error);
      loadPage();
    }
  };

  const retryHistoryEntry = async (id: number) => {
    const result = await commands.retryHistoryEntryTranscription(id);
    if (result.status !== "ok") {
      throw new Error(String(result.error));
    }
  };

  const openRecordingsFolder = async () => {
    try {
      const result = await commands.openRecordingsFolder();
      if (result.status !== "ok") {
        throw new Error(String(result.error));
      }
    } catch (error) {
      console.error("Failed to open recordings folder:", error);
    }
  };

  let content: React.ReactNode;

  if (loading) {
    content = (
      <div className="px-4 py-3 text-center text-text/60">
        {t("settings.history.loading")}
      </div>
    );
  } else if (entries.length === 0) {
    content = (
      <div className="px-6 py-14 text-center font-display text-lg text-text/55">
        {t("settings.history.empty")}
      </div>
    );
  } else {
    content = (
      <>
        <AudioPlayerGroup>
          <div className="divide-y divide-stone/15">
            {entries.map((entry) => (
              <HistoryEntryComponent
                key={entry.id}
                entry={entry}
                onToggleSaved={() => toggleSaved(entry.id)}
                onCopyText={() => copyToClipboard(pastedText(entry))}
                getAudioUrl={getAudioUrl}
                deleteAudio={deleteAudioEntry}
                retryTranscription={retryHistoryEntry}
              />
            ))}
          </div>
        </AudioPlayerGroup>
        {/* Sentinel for infinite scroll */}
        <div ref={sentinelRef} className="h-1" />
      </>
    );
  }

  return (
    <div className="max-w-2xl w-full mx-auto space-y-6">
      <PageHeader
        title={t("settings.history.title")}
        description={t("settings.history.description")}
        actions={
          <OpenRecordingsButton
            onClick={openRecordingsFolder}
            label={t("settings.history.openFolder")}
          />
        }
      />
      <div className="bg-surface border border-stone/20 rounded-xl overflow-visible divide-y divide-stone/15">
        {content}
      </div>
      <SettingsGroup title={t("settings.history.keep.title")}>
        <HistoryRetention descriptionMode="tooltip" grouped={true} />
      </SettingsGroup>
    </div>
  );
};

/** What was pasted: the cleaned-up text when there is one, else the raw transcript. */
const pastedText = (entry: HistoryEntry) =>
  entry.post_processed_text?.trim()
    ? entry.post_processed_text
    : entry.transcription_text;

interface HistoryEntryProps {
  entry: HistoryEntry;
  onToggleSaved: () => void;
  onCopyText: () => Promise<boolean>;
  getAudioUrl: (fileName: string) => Promise<string | null>;
  deleteAudio: (id: number) => Promise<void>;
  retryTranscription: (id: number) => Promise<void>;
}

const HistoryEntryComponent: React.FC<HistoryEntryProps> = ({
  entry,
  onToggleSaved,
  onCopyText,
  getAudioUrl,
  deleteAudio,
  retryTranscription,
}) => {
  const { t, i18n } = useTranslation();
  const [showCopied, setShowCopied] = useState(false);
  const [retrying, setRetrying] = useState(false);

  const [showOriginal, setShowOriginal] = useState(false);
  const [showContext, setShowContext] = useState(false);
  const { app_name, url_host, category, cleanup_context } = entry.context;
  // "Claude · Coding agents", or "Chrome (mail.google.com) · Email".
  const where = [
    app_name && (url_host ? `${app_name} (${url_host})` : app_name),
    category && t(`settings.style.categories.${category}`),
  ]
    .filter(Boolean)
    .join(" · ");
  const hasTranscription = pastedText(entry).trim().length > 0;
  const hasOriginal =
    !!entry.post_processed_text?.trim() &&
    entry.post_processed_text !== entry.transcription_text;

  const handleLoadAudio = useCallback(
    () => getAudioUrl(entry.file_name),
    [getAudioUrl, entry.file_name],
  );

  const handleCopyText = async () => {
    if (!hasTranscription) {
      return;
    }

    const copied = await onCopyText();
    if (!copied) {
      toast.error(t("settings.history.copyError"));
      return;
    }

    setShowCopied(true);
    setTimeout(() => setShowCopied(false), 2000);
  };

  const handleDeleteEntry = async () => {
    try {
      await deleteAudio(entry.id);
    } catch (error) {
      console.error("Failed to delete entry:", error);
      toast.error(t("settings.history.deleteError"));
    }
  };

  const handleRetranscribe = async () => {
    try {
      setRetrying(true);
      await retryTranscription(entry.id);
    } catch (error) {
      console.error("Failed to re-transcribe:", error);
      toast.error(t("settings.history.retranscribeError"));
    } finally {
      setRetrying(false);
    }
  };

  const formattedDate = formatDateTime(String(entry.timestamp), i18n.language);

  return (
    <div className="px-4 pt-3 pb-4 flex flex-col gap-2">
      <div className="flex justify-between items-center">
        <p className="text-xs text-text/50">
          <span className="tabular-nums">{formattedDate}</span>
          {where && ` · ${where}`}
        </p>
        <div className="flex items-center">
          <IconButton
            onClick={handleCopyText}
            disabled={!hasTranscription || retrying}
            title={t("settings.history.copyToClipboard")}
          >
            {showCopied ? (
              <Check width={16} height={16} />
            ) : (
              <Copy width={16} height={16} />
            )}
          </IconButton>
          <IconButton
            onClick={onToggleSaved}
            disabled={retrying}
            active={entry.saved}
            title={
              entry.saved
                ? t("settings.history.unsave")
                : t("settings.history.save")
            }
          >
            <Star
              width={16}
              height={16}
              fill={entry.saved ? "currentColor" : "none"}
            />
          </IconButton>
          <IconButton
            onClick={handleRetranscribe}
            disabled={retrying || !entry.has_audio}
            title={
              entry.has_audio
                ? t("settings.history.retranscribe")
                : t("settings.history.retention.noAudioForEntry")
            }
          >
            <RotateCcw
              width={16}
              height={16}
              style={
                retrying
                  ? { animation: "spin 1s linear infinite reverse" }
                  : undefined
              }
            />
          </IconButton>
          <IconButton
            onClick={handleDeleteEntry}
            disabled={retrying}
            title={t("settings.history.delete")}
          >
            <Trash2 width={16} height={16} />
          </IconButton>
        </div>
      </div>

      <p
        className={`font-display text-[16px] leading-relaxed pb-1 ${
          retrying
            ? ""
            : hasTranscription
              ? "text-text/90 select-text cursor-text whitespace-pre-wrap break-words"
              : "text-text/40"
        }`}
        style={
          retrying
            ? { animation: "transcribe-pulse 3s ease-in-out infinite" }
            : undefined
        }
      >
        {retrying && (
          <style>{`
            @keyframes transcribe-pulse {
              0%, 100% { color: color-mix(in srgb, var(--color-text) 40%, transparent); }
              50% { color: color-mix(in srgb, var(--color-text) 90%, transparent); }
            }
          `}</style>
        )}
        {retrying
          ? t("settings.history.transcribing")
          : hasTranscription
            ? pastedText(entry)
            : t("settings.history.transcriptionFailed")}
      </p>

      {(hasOriginal || cleanup_context) && !retrying && (
        <div className="pb-2 -mt-1 space-y-1">
          <div className="flex gap-3">
            {hasOriginal && (
              <button
                onClick={() => setShowOriginal(!showOriginal)}
                className="text-xs text-text/50 hover:text-text cursor-pointer"
              >
                {showOriginal
                  ? t("settings.history.hideOriginal")
                  : t("settings.history.showOriginal")}
              </button>
            )}
            {cleanup_context && (
              <button
                onClick={() => setShowContext(!showContext)}
                className="text-xs text-text/50 hover:text-text cursor-pointer"
              >
                {showContext
                  ? t("settings.history.hideContext")
                  : t("settings.history.showContext")}
              </button>
            )}
          </div>
          {showOriginal && (
            <p className="text-xs text-text/60 select-text cursor-text whitespace-pre-wrap break-words border-s-2 border-stone/20 ps-2">
              {entry.transcription_text}
            </p>
          )}
          {showContext && cleanup_context && (
            <pre className="max-h-64 overflow-auto rounded-md bg-sunken p-2 font-mono text-xs leading-snug text-text/70 select-text whitespace-pre-wrap break-words">
              {cleanup_context}
            </pre>
          )}
        </div>
      )}

      {entry.has_audio && (
        <AudioPlayer onLoadRequest={handleLoadAudio} className="w-full" />
      )}
    </div>
  );
};
