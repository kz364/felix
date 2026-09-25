import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown, Mic, Trash2 } from "lucide-react";
import { commands, type Report } from "@/bindings";

/** Past mistake reports: what was said, which model, what changed. */
export const MistakeReports: React.FC<{ version: number }> = ({ version }) => {
  const { t, i18n } = useTranslation();
  const [reports, setReports] = useState<Report[]>([]);
  const [open, setOpen] = useState<string | null>(null);

  const load = useCallback(() => {
    commands.mistakeReports().then(setReports);
  }, []);
  useEffect(load, [load, version]);

  if (reports.length === 0) return null;

  const when = (at: string) => {
    const date = new Date(at);
    return Number.isNaN(date.getTime())
      ? ""
      : date.toLocaleString(i18n.language, {
          dateStyle: "medium",
          timeStyle: "short",
        });
  };

  const statusClass: Record<string, string> = {
    applied: "text-success",
    proposed: "text-text/50",
    "no change": "text-text/50",
    failed: "text-error",
  };

  return (
    <div className="divide-y divide-stone/15">
      {reports.map((r) => {
        const expanded = open === r.id;
        return (
          <div key={r.id} className="px-4 py-2.5">
            <div className="flex items-start gap-2">
              <button
                type="button"
                onClick={() => setOpen(expanded ? null : r.id)}
                className="flex min-w-0 flex-1 items-start gap-2 text-start cursor-pointer"
              >
                <ChevronDown
                  className={`mt-0.5 h-4 w-4 shrink-0 text-text/40 transition-transform ${expanded ? "" : "-rotate-90"}`}
                />
                <div className="min-w-0 flex-1">
                  <div className="text-sm">{r.report}</div>
                  <div className="mt-0.5 flex flex-wrap items-center gap-x-2 text-xs text-text/50">
                    <span>{when(r.at)}</span>
                    {r.source === "voice" && (
                      <span>{t("settings.vocabulary.reports.byVoice")}</span>
                    )}
                    {r.transcription_model && (
                      <span>{r.transcription_model}</span>
                    )}
                    {r.has_audio && (
                      <Mic
                        className="h-3 w-3"
                        aria-label={t("settings.vocabulary.reports.hasAudio")}
                      />
                    )}
                    <span className={statusClass[r.status] ?? ""}>
                      {t(`settings.vocabulary.reports.status.${r.status}`, {
                        defaultValue: r.status,
                      })}
                    </span>
                  </div>
                </div>
              </button>
              <button
                type="button"
                onClick={async () => {
                  await commands.forgetMistakeReport(r.id);
                  load();
                }}
                title={t("settings.vocabulary.reports.delete")}
                aria-label={t("settings.vocabulary.reports.delete")}
                className="rounded p-1 text-text/40 hover:text-error hover:bg-error/10 cursor-pointer"
              >
                <Trash2 className="h-3.5 w-3.5" />
              </button>
            </div>
            {expanded && (
              <div className="ms-6 mt-2 space-y-2 text-xs">
                {r.transcribed && (
                  <div>
                    <div className="text-text/50">
                      {t("settings.vocabulary.reports.transcribed")}
                    </div>
                    <div>{r.transcribed}</div>
                  </div>
                )}
                {r.pasted && r.pasted !== r.transcribed && (
                  <div>
                    <div className="text-text/50">
                      {t("settings.vocabulary.reports.pasted")}
                    </div>
                    <div>{r.pasted}</div>
                  </div>
                )}
                {r.explanation && <p className="text-sm">{r.explanation}</p>}
                {r.error && <p className="text-error">{r.error}</p>}
                {r.change.length > 0 && (
                  <pre className="max-h-48 overflow-auto rounded-md bg-stone/10 p-2 leading-snug">
                    {r.change.map((l, i) => (
                      <div
                        key={i}
                        className={
                          l.startsWith("+") ? "text-success" : "text-error"
                        }
                      >
                        {l}
                      </div>
                    ))}
                  </pre>
                )}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
};
