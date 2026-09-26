import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { X } from "lucide-react";
import { commands } from "@/bindings";
import type { AgentRunRecord } from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";

/** Apps Felix may always use, or never, each removable. */
export const AgentApps: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, refreshSettings } = useSettings();
  const apps = getSetting("agent_app_access") ?? [];

  const remove = async (key: string) => {
    await commands.removeAgentAppAccess(key);
    await refreshSettings();
  };

  const group = (allowed: boolean) => {
    const list = apps.filter((a) => a.allowed === allowed);
    if (list.length === 0) return null;
    return (
      <div className="space-y-1">
        <div className="text-sm text-text/60">
          {t(
            `settings.voiceControl.assistant.computer.apps.${allowed ? "allowed" : "denied"}`,
          )}
        </div>
        <div className="flex flex-wrap gap-1.5">
          {list.map((a) => {
            const key = a.bundle_id || a.name.toLowerCase();
            return (
              <span
                key={key}
                className="inline-flex items-center gap-1 rounded-full border border-stone/30 bg-stone/10 ps-2.5 pe-1 py-0.5 text-sm"
              >
                {a.name}
                <button
                  className="rounded p-0.5 text-text/40 hover:text-error hover:bg-error/10 cursor-pointer"
                  aria-label={t(
                    "settings.voiceControl.assistant.computer.apps.remove",
                    { app: a.name },
                  )}
                  onClick={() => remove(key)}
                >
                  <X className="h-3.5 w-3.5" />
                </button>
              </span>
            );
          })}
        </div>
      </div>
    );
  };

  return (
    <div className="px-4 py-3 space-y-2">
      <div>
        <h3 className="text-sm font-medium">
          {t("settings.voiceControl.assistant.computer.apps.title")}
        </h3>
        <p className="mt-0.5 text-sm leading-snug text-text/60">
          {t("settings.voiceControl.assistant.computer.apps.description")}
        </p>
      </div>
      {apps.length === 0 ? (
        <p className="text-sm text-text/50">
          {t("settings.voiceControl.assistant.computer.apps.empty")}
        </p>
      ) : (
        <>
          {group(true)}
          {group(false)}
        </>
      )}
    </div>
  );
};

/** What Felix did in recent tasks, step by step. */
export const AgentRuns: React.FC = () => {
  const { t } = useTranslation();
  const [runs, setRuns] = useState<AgentRunRecord[]>([]);
  const [open, setOpen] = useState<number | null>(null);

  useEffect(() => {
    commands.recentAgentRuns().then(setRuns);
  }, []);

  return (
    <div className="px-4 py-3 space-y-2">
      <h3 className="text-sm font-medium">
        {t("settings.voiceControl.assistant.computer.runs.title")}
      </h3>
      {runs.length === 0 ? (
        <p className="text-sm text-text/50">
          {t("settings.voiceControl.assistant.computer.runs.empty")}
        </p>
      ) : (
        <ul className="space-y-1">
          {runs.map((run, i) => (
            <li key={`${run.at}-${i}`}>
              <button
                className="w-full text-start rounded-md px-2 py-1 hover:bg-stone/10 cursor-pointer"
                onClick={() => setOpen(open === i ? null : i)}
              >
                <div className="text-sm truncate">{run.task}</div>
                <div className="text-xs text-text/50">
                  {new Date(run.at).toLocaleString()} ·{" "}
                  {t(
                    `settings.voiceControl.assistant.computer.runs.status.${run.status}`,
                  )}{" "}
                  ·{" "}
                  {t("settings.voiceControl.assistant.computer.runs.steps", {
                    count: run.steps.length,
                  })}
                </div>
              </button>
              {open === i && (
                <div className="ms-2 mt-1 mb-2 space-y-1 border-s-2 border-stone/20 ps-3">
                  <ol className="list-decimal ms-4 space-y-0.5 text-sm text-text/70">
                    {run.steps.map((s, j) => (
                      <li key={j}>{s}</li>
                    ))}
                  </ol>
                  {run.message && (
                    <p className="text-sm text-text/60 whitespace-pre-wrap">
                      {run.message}
                    </p>
                  )}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
};
