import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands, type ExtensionStatus } from "@/bindings";
import { SettingContainer } from "../ui/SettingContainer";
import { Button } from "../ui/Button";

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
      ? t("meetings.settings.extension.working", { app: status?.app ?? "" })
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
