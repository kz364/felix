import React from "react";
import { useTranslation } from "react-i18next";
import {
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
  /** Pinned to the bottom of the sidebar with the other housekeeping pages. */
  bottom?: boolean;
}

export const SECTIONS_CONFIG = {
  dictation: {
    labelKey: "sidebar.dictation",
    icon: Mic,
    component: DictationSettings,
    enabled: () => true,
  },
  history: {
    labelKey: "sidebar.history",
    icon: History,
    component: HistorySettings,
    enabled: () => true,
  },
  vocabulary: {
    labelKey: "sidebar.vocabulary",
    icon: BookA,
    component: VocabularySettings,
    enabled: () => true,
  },
  writing: {
    labelKey: "sidebar.writing",
    icon: PenLine,
    component: WritingSettings,
    enabled: () => true,
  },
  voice: {
    labelKey: "sidebar.voiceCommands",
    icon: AudioLines,
    component: VoiceCommandsSettings,
    enabled: () => true,
  },
  felix: {
    labelKey: "sidebar.felix",
    icon: Bot,
    component: FelixSettings,
    enabled: () => true,
  },
  meetings: {
    labelKey: "sidebar.meetings",
    icon: NotebookPen,
    component: MeetingsPage,
    enabled: () => true,
  },
  models: {
    labelKey: "sidebar.models",
    icon: Cpu,
    component: ModelsSettings,
    enabled: () => true,
    bottom: true,
  },
  settings: {
    labelKey: "sidebar.settings",
    icon: Settings2,
    component: AppSettings,
    enabled: () => true,
    bottom: true,
  },
  debug: {
    labelKey: "sidebar.debug",
    icon: FlaskConical,
    component: DebugSettings,
    enabled: (settings) => settings?.debug_mode ?? false,
    bottom: true,
  },
} as const satisfies Record<string, SectionConfig>;

interface SidebarProps {
  activeSection: SidebarSection;
  onSectionChange: (section: SidebarSection) => void;
}

export const Sidebar: React.FC<SidebarProps> = ({
  activeSection,
  onSectionChange,
}) => {
  const { t } = useTranslation();
  const { settings } = useSettings();

  const sections = Object.entries(SECTIONS_CONFIG)
    .filter(([_, config]) => config.enabled(settings))
    .map(([id, config]) => ({
      id: id as SidebarSection,
      bottom: "bottom" in config && config.bottom,
      ...config,
    }));

  const item = (section: (typeof sections)[number]) => {
    const Icon = section.icon;
    const isActive = activeSection === section.id;
    return (
      <button
        key={section.id}
        type="button"
        aria-current={isActive ? "page" : undefined}
        className={`flex w-full items-center gap-2.5 rounded-lg px-2.5 h-8 text-[13px] text-start transition-colors duration-150 cursor-pointer ${
          isActive
            ? "bg-surface text-text font-medium shadow-sm ring-1 ring-stone/15"
            : "text-text/65 hover:text-text hover:bg-stone/10"
        }`}
        onClick={() => onSectionChange(section.id)}
      >
        <Icon width={16} height={16} strokeWidth={1.75} className="shrink-0" />
        <span className="truncate" title={t(section.labelKey)}>
          {t(section.labelKey)}
        </span>
      </button>
    );
  };

  return (
    <nav className="flex flex-col w-52 shrink-0 h-full bg-sunken border-e border-stone/15 px-3 pb-3">
      <div className="px-2 pt-5 pb-5">
        <Wordmark />
      </div>
      <div className="flex flex-col gap-0.5">
        {sections.filter((s) => !s.bottom).map(item)}
      </div>
      <div className="mt-auto flex flex-col gap-0.5 pt-3">
        {sections.filter((s) => s.bottom).map(item)}
      </div>
    </nav>
  );
};
