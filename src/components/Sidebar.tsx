import React from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowLeft,
  AudioLines,
  BookA,
  Bot,
  Cpu,
  FlaskConical,
  History,
  Mic,
  NotebookPen,
  PenLine,
  Settings2,
} from "lucide-react";
import Wordmark from "./icons/Wordmark";
import { useSettings } from "../hooks/useSettings";
import { MeetingsPage } from "./meetings/MeetingsPage";
import { MeetingSettingsPage } from "./meetings/MeetingSettingsPage";
import {
  AppSettings,
  DebugSettings,
  DictationSettings,
  FelixSettings,
  HistorySettings,
  ModelsSettings,
  VocabularySettings,
  VoiceCommandsSettings,
  WritingSettings,
} from "./settings";

export type SidebarSection = keyof typeof SECTIONS_CONFIG;

interface IconProps {
  width?: number | string;
  height?: number | string;
  size?: number | string;
  className?: string;
  [key: string]: any;
}

interface SectionConfig {
  labelKey: string;
  icon: React.ComponentType<IconProps>;
  component: React.ComponentType;
  enabled: (settings: any) => boolean;
  /** The main sidebar, or the settings one (which replaces it). */
  area: "app" | "settings";
}

export const SECTIONS_CONFIG = {
  // What you use: your meetings, dictations and words.
  meetings: {
    labelKey: "sidebar.meetings",
    icon: NotebookPen,
    component: MeetingsPage,
    enabled: () => true,
    area: "app",
  },
  history: {
    labelKey: "sidebar.history",
    icon: History,
    component: HistorySettings,
    enabled: () => true,
    area: "app",
  },
  vocabulary: {
    labelKey: "sidebar.vocabulary",
    icon: BookA,
    component: VocabularySettings,
    enabled: () => true,
    area: "app",
  },
  // Settings, each in one place even when dictation and meetings share it.
  dictation: {
    labelKey: "sidebar.dictation",
    icon: Mic,
    component: DictationSettings,
    enabled: () => true,
    area: "settings",
  },
  writing: {
    labelKey: "sidebar.writing",
    icon: PenLine,
    component: WritingSettings,
    enabled: () => true,
    area: "settings",
  },
  voice: {
    labelKey: "sidebar.voiceCommands",
    icon: AudioLines,
    component: VoiceCommandsSettings,
    enabled: () => true,
    area: "settings",
  },
  felix: {
    labelKey: "sidebar.felix",
    icon: Bot,
    component: FelixSettings,
    enabled: () => true,
    area: "settings",
  },
  meetingSettings: {
    labelKey: "sidebar.meetings",
    icon: NotebookPen,
    component: MeetingSettingsPage,
    enabled: () => true,
    area: "settings",
  },
  models: {
    labelKey: "sidebar.models",
    icon: Cpu,
    component: ModelsSettings,
    enabled: () => true,
    area: "settings",
  },
  settings: {
    labelKey: "sidebar.general",
    icon: Settings2,
    component: AppSettings,
    enabled: () => true,
    area: "settings",
  },
  debug: {
    labelKey: "sidebar.debug",
    icon: FlaskConical,
    component: DebugSettings,
    enabled: (settings) => settings?.debug_mode ?? false,
    area: "settings",
  },
} as const satisfies Record<string, SectionConfig>;

export const isSettingsSection = (section: SidebarSection) =>
  SECTIONS_CONFIG[section].area === "settings";

interface SidebarProps {
  activeSection: SidebarSection;
  onSectionChange: (section: SidebarSection) => void;
  /** Open settings where they were last left. */
  onOpenSettings: () => void;
  /** Leave settings for the page before them. */
  onCloseSettings: () => void;
}

/** The main sidebar (meetings, history, vocabulary, and Settings at the
 *  bottom), or while in settings, the settings pages with a way back. */
export const Sidebar: React.FC<SidebarProps> = ({
  activeSection,
  onSectionChange,
  onOpenSettings,
  onCloseSettings,
}) => {
  const { t } = useTranslation();
  const { settings } = useSettings();
  const inSettings = isSettingsSection(activeSection);

  const sections = Object.entries(SECTIONS_CONFIG)
    .filter(([_, config]) => config.enabled(settings))
    .map(([id, config]) => ({ id: id as SidebarSection, ...config }))
    .filter((s) => (s.area === "settings") === inSettings);

  const row = (
    key: string,
    Icon: React.ComponentType<IconProps>,
    label: string,
    isActive: boolean,
    onClick: () => void,
  ) => (
    <button
      key={key}
      type="button"
      aria-current={isActive ? "page" : undefined}
      className={`flex w-full items-center gap-2.5 rounded-lg px-2.5 h-8 text-sm text-start transition-colors duration-150 cursor-pointer ${
        isActive
          ? "bg-surface text-text font-medium shadow-sm ring-1 ring-stone/15"
          : "text-text/65 hover:text-text hover:bg-stone/10"
      }`}
      onClick={onClick}
    >
      <Icon width={16} height={16} strokeWidth={1.75} className="shrink-0" />
      <span className="truncate" title={label}>
        {label}
      </span>
    </button>
  );

  const items = sections.map((section) =>
    row(
      section.id,
      section.icon,
      t(section.labelKey),
      activeSection === section.id,
      () => onSectionChange(section.id),
    ),
  );

  if (inSettings) {
    return (
      <nav className="flex flex-col w-52 shrink-0 h-full bg-sunken border-e border-stone/15 px-3 pb-3">
        <div className="pt-4 pb-3">
          {row("back", ArrowLeft, t("sidebar.back"), false, onCloseSettings)}
        </div>
        <div className="px-2.5 pb-2 text-xs font-medium uppercase tracking-wide text-text/45">
          {t("sidebar.settings")}
        </div>
        <div className="flex flex-col gap-0.5">{items}</div>
      </nav>
    );
  }

  return (
    <nav className="flex flex-col w-52 shrink-0 h-full bg-sunken border-e border-stone/15 px-3 pb-3">
      <div className="px-2 pt-5 pb-5">
        <Wordmark />
      </div>
      <div className="flex flex-col gap-0.5">{items}</div>
      <div className="mt-auto flex flex-col gap-0.5 pt-3">
        {row(
          "settings",
          Settings2,
          t("sidebar.settings"),
          false,
          onOpenSettings,
        )}
      </div>
    </nav>
  );
};
