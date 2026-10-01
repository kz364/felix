import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  commands,
  type ExtensionStatus,
  type RememberedVoices as Remembered,
} from "@/bindings";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { SettingContainer } from "../ui/SettingContainer";
import { Button } from "../ui/Button";

/** How the extension names the call page it's on. */
const APP_NAMES: Record<string, string> = {
  meet: "Google Meet",
  zoom: "Zoom",
  teams: "Teams",
};

/** Settings → Meetings → Speakers: the Chrome extension that reads who's
 *  talking on Meet. Loaded unpacked; Felix puts its helper in Chrome's
 *  folder when asked. */
export const ChromeExtension: React.FC = () => {
  const { t } = useTranslation();
  const [status, setStatus] = useState<ExtensionStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => {
    commands.extensionStatus().then(setStatus);
  }, []);
  useEffect(() => {
    refresh();
    const id = setInterval(refresh, 5000);
    return () => clearInterval(id);
  }, [refresh]);

  const setUp = async () => {
    setBusy(true);
    setError(null);
    const r = await commands.installExtensionHost();
    if (r.status === "error") setError(r.error);
    setBusy(false);
    refresh();
  };

  const heard =
    status?.heard_secs_ago != null && status.heard_secs_ago < 15 * 60;
  const state = !status?.host_installed
    ? t("meetings.settings.extension.notSetUp")
    : heard
      ? t("meetings.settings.extension.working", {
          app: APP_NAMES[status?.app ?? ""] ?? status?.app ?? "",
        })
      : t("meetings.settings.extension.waiting");

  return (
    <SettingContainer
      title={t("meetings.settings.extension.title")}
      description={t("meetings.settings.extension.description")}
      descriptionMode="inline"
      layout="stacked"
      grouped
    >
      <div className="space-y-2 text-sm">
        <p className={heard ? "text-green-600" : "text-text/70"}>{state}</p>
        <ol className="list-decimal pl-5 space-y-1 text-text/70">
          <li>{t("meetings.settings.extension.step1")}</li>
          <li>{t("meetings.settings.extension.step2")}</li>
          <li>{t("meetings.settings.extension.step3")}</li>
        </ol>
        <div className="flex gap-2">
          <Button size="sm" onClick={setUp} disabled={busy}>
            {status?.host_installed
              ? t("meetings.settings.extension.setUpAgain")
              : t("meetings.settings.extension.setUp")}
          </Button>
          <Button
            size="sm"
            variant="secondary"
            onClick={() => commands.revealExtensionFolder()}
          >
            {t("meetings.settings.extension.showFolder")}
          </Button>
        </div>
        {error && <p className="text-red-600">{error}</p>}
      </div>
    </SettingContainer>
  );
};

/** Calendar access, so the people invited to a meeting's event can name
 *  the voices nobody else did. Felix asks only from this button. */
export const CalendarAccess: React.FC = () => {
  const { t } = useTranslation();
  const [access, setAccess] = useState<string | null>(null);
  useEffect(() => {
    commands.calendarAccess().then(setAccess);
  }, []);
  const ask = async () => setAccess(await commands.requestCalendarAccess());
  const state =
    access === "full"
      ? t("meetings.settings.calendar.on")
      : access === "denied" || access === "restricted"
        ? t("meetings.settings.calendar.denied")
        : t("meetings.settings.calendar.off");
  return (
    <SettingContainer
      title={t("meetings.settings.calendar.title")}
      description={t("meetings.settings.calendar.description")}
      descriptionMode="inline"
      grouped
    >
      <div className="flex items-center gap-3 text-sm">
        <span className={access === "full" ? "text-green-600" : "text-text/70"}>
          {state}
        </span>
        {access !== "full" &&
          access !== "denied" &&
          access !== "restricted" && (
            <Button size="sm" onClick={ask}>
              {t("meetings.settings.calendar.allow")}
            </Button>
          )}
      </div>
    </SettingContainer>
  );
};

/** Voices remembered across meetings: rename, merge two that are one
 *  person, forget one, or stop remembering. Stored on this Mac only. */
export const RememberedVoices: React.FC = () => {
  const { t } = useTranslation();
  const [data, setData] = useState<Remembered | null>(null);
  const [editing, setEditing] = useState<number | null>(null);
  const [draft, setDraft] = useState("");
  const [merging, setMerging] = useState<number | null>(null);

  const load = useCallback(() => {
    commands.rememberedVoices().then((r) => {
      if (r.status === "ok") setData(r.data);
    });
  }, []);
  useEffect(load, [load]);

  const label = (v: Remembered["voices"][number]) =>
    v.name ?? t("meetings.settings.remembered.unknown", { n: v.number });

  const save = async (id: number) => {
    await commands.renameRememberedVoice(id, draft);
    setEditing(null);
    load();
  };

  return (
    <>
      <ToggleSwitch
        checked={data?.enabled ?? true}
        onChange={async (on) => {
          await commands.setRememberVoices(on);
          load();
        }}
        isUpdating={data === null}
        label={t("meetings.settings.remembered.title")}
        description={t("meetings.settings.remembered.description")}
        descriptionMode="inline"
        grouped
      />
      {data?.enabled && data.voices.length > 0 && (
        <SettingContainer
          title={t("meetings.settings.remembered.listTitle", {
            count: data.voices.length,
          })}
          description={t("meetings.settings.remembered.listDescription")}
          descriptionMode="inline"
          layout="stacked"
          grouped
        >
          <ul className="divide-y divide-stone/15 text-sm">
            {data.voices.map((v) => (
              <li key={v.id} className="flex items-center gap-2 py-1.5">
                {editing === v.id ? (
                  <input
                    autoFocus
                    value={draft}
                    onChange={(e) => setDraft(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") save(v.id);
                      if (e.key === "Escape") setEditing(null);
                    }}
                    onBlur={() => save(v.id)}
                    placeholder={t(
                      "meetings.settings.remembered.namePlaceholder",
                    )}
                    className="flex-1 rounded border border-stone/25 bg-surface px-2 py-0.5 outline-none focus:border-accent/60"
                  />
                ) : (
                  <span
                    className={`flex-1 ${v.name ? "" : "text-text/55 italic"}`}
                  >
                    {label(v)}
                  </span>
                )}
                <span className="text-text/45 text-xs whitespace-nowrap">
                  {t("meetings.settings.remembered.meetings", {
                    count: v.meetings,
                  })}
                </span>
                {merging !== null && merging !== v.id ? (
                  <Button
                    size="sm"
                    variant="primary-soft"
                    onClick={async () => {
                      await commands.mergeRememberedVoices(v.id, merging);
                      setMerging(null);
                      load();
                    }}
                  >
                    {t("meetings.settings.remembered.mergeInto")}
                  </Button>
                ) : merging === v.id ? (
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => setMerging(null)}
                  >
                    {t("meetings.settings.remembered.cancel")}
                  </Button>
                ) : (
                  <>
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => {
                        setDraft(v.name ?? "");
                        setEditing(v.id);
                      }}
                    >
                      {t("meetings.settings.remembered.rename")}
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => setMerging(v.id)}
                    >
                      {t("meetings.settings.remembered.merge")}
                    </Button>
                    <Button
                      size="sm"
                      variant="danger-ghost"
                      onClick={async () => {
                        await commands.deleteRememberedVoice(v.id);
                        load();
                      }}
                    >
                      {t("meetings.settings.remembered.forget")}
                    </Button>
                  </>
                )}
              </li>
            ))}
          </ul>
        </SettingContainer>
      )}
    </>
  );
};
