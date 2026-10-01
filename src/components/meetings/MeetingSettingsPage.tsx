import React from "react";
import { useTranslation } from "react-i18next";
import { PageHeader } from "../ui/PageHeader";
import { SettingsGroup } from "../ui/SettingsGroup";
import { ShortcutInput } from "../settings/ShortcutInput";
import { MeetingSettings } from "./MeetingSettings";
import { CalendarAccess, ChromeExtension } from "./SpeakerSettings";

/** Settings → Meetings: how meetings are recorded, transcribed and summed up. */
export const MeetingSettingsPage: React.FC = () => {
  const { t } = useTranslation();
  return (
    <div className="max-w-2xl w-full mx-auto space-y-6">
      <PageHeader
        title={t("meetings.settings.title")}
        description={t("meetings.settings.description")}
      />
      <SettingsGroup>
        <ShortcutInput shortcutId="meeting" grouped={true} />
        <MeetingSettings />
      </SettingsGroup>
      <SettingsGroup
        title={t("meetings.settings.speakers.title")}
        description={t("meetings.settings.speakers.description")}
      >
        <ChromeExtension />
        <CalendarAccess />
      </SettingsGroup>
    </div>
  );
};
