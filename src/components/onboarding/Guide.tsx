import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check } from "lucide-react";
import { commands } from "@/bindings";
import Wordmark from "../icons/Wordmark";
import { Button } from "../ui/Button";
import { Textarea } from "../ui/Textarea";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { ShortcutInput } from "../settings/ShortcutInput";
import { ChatGptAccount } from "../settings/voice-control/AssistantSettings";
import { useSettings } from "../../hooks/useSettings";
import { TeachWord } from "../settings/vocabulary/TeachWord";
import { LiveDraftSetup } from "../settings/LiveDraft";

/** Settings → "Show the guided tour" opens the tour with this event. */
export const OPEN_GUIDE_EVENT = "felix-open-guide";

const STEPS = ["dictate", "draft", "teach", "chatgpt", "felix"] as const;
type Step = (typeof STEPS)[number];
/** Steps that use the scratchpad beside them. */
const WITH_SCRATCHPAD: Step[] = ["dictate", "draft", "felix"];

/**
 * A short guided tour after setup (and from Settings later): try a
 * dictation, set up the live draft, teach a word, sign in to ChatGPT, meet
 * the assistant. Every step can be skipped. Dictation steps have a
 * scratchpad beside them to watch it happen.
 */
const Guide: React.FC<{ onDone: () => void }> = ({ onDone }) => {
  const { t } = useTranslation();
  const { getSetting } = useSettings();
  const name = getSetting("assistant_name") ?? "Felix";
  const [index, setIndex] = useState(0);
  const step: Step = STEPS[index];
  const last = index === STEPS.length - 1;
  const next = () => (last ? onDone() : setIndex(index + 1));
  const [scratch, setScratch] = useState("");
  const scratchpad = WITH_SCRATCHPAD.includes(step);

  return (
    <div className="h-screen w-full flex flex-col p-6 gap-6 overflow-y-auto">
      <div className="flex items-center justify-between shrink-0">
        <Wordmark size={32} />
        <button
          type="button"
          onClick={onDone}
          className="text-sm text-text/60 hover:text-text cursor-pointer"
        >
          {t("onboarding.guide.skipAll")}
        </button>
      </div>

      <div
        className={`w-full mx-auto flex-1 flex gap-6 ${scratchpad ? "max-w-4xl" : "max-w-lg"}`}
      >
        <div className="flex-1 min-w-0 flex flex-col gap-5">
          <div className="flex gap-1.5" aria-hidden="true">
            {STEPS.map((s, i) => (
              <span
                key={s}
                className={`h-1 flex-1 rounded-full ${i <= index ? "bg-accent" : "bg-stone/25"}`}
              />
            ))}
          </div>
          <div>
            <p className="text-sm text-text/50">
              {t("onboarding.guide.stepOf", {
                n: index + 1,
                total: STEPS.length,
              })}
            </p>
            <h2 className="font-display text-[28px] leading-tight text-text mt-1">
              {t(`onboarding.guide.${step}.title`, { name })}
            </h2>
            <p className="text-text/70 mt-2">
              {t(`onboarding.guide.${step}.description`, { name })}
            </p>
          </div>

          {step === "dictate" && <DictateStep done={scratch.trim() !== ""} />}
          {step === "draft" && <LiveDraftSetup showReady />}
          {step === "teach" && <TeachWord autoFocus />}
          {step === "chatgpt" && <ChatGptStep />}
          {step === "felix" && <FelixStep />}

          <div className="mt-auto flex items-center justify-between pt-2">
            <Button
              variant="ghost"
              onClick={() => setIndex(index - 1)}
              disabled={index === 0}
            >
              {t("onboarding.guide.back")}
            </Button>
            <div className="flex gap-2">
              {!last && (
                <Button variant="secondary" onClick={next}>
                  {t("onboarding.guide.skip")}
                </Button>
              )}
              <Button onClick={next}>
                {last
                  ? t("onboarding.guide.finish")
                  : t("onboarding.guide.next")}
              </Button>
            </div>
          </div>
        </div>
        {scratchpad && (
          <div className="w-80 shrink-0 flex flex-col gap-2">
            <span className="text-sm font-medium text-text/70">
              {t("onboarding.guide.scratchpad.title")}
            </span>
            <Textarea
              aria-label={t("onboarding.guide.scratchpad.title")}
              autoFocus
              value={scratch}
              onChange={(e) => setScratch(e.target.value)}
              placeholder={t(`onboarding.guide.scratchpad.${step}`, { name })}
              className="w-full flex-1 min-h-64 resize-none"
            />
          </div>
        )}
      </div>
    </div>
  );
};

const Card: React.FC<{ children: React.ReactNode }> = ({ children }) => (
  <div className="rounded-xl border border-stone/20 bg-surface">{children}</div>
);

const DictateStep: React.FC<{ done: boolean }> = ({ done }) => {
  const { t } = useTranslation();
  return (
    <div className="space-y-3">
      <Card>
        <ShortcutInput shortcutId="transcribe" grouped={true} />
      </Card>
      {done && (
        <p className="flex items-center gap-1.5 text-sm text-success">
          <Check className="h-4 w-4" />
          {t("onboarding.guide.dictate.worked")}
        </p>
      )}
    </div>
  );
};

const ChatGptStep: React.FC = () => {
  const { t } = useTranslation();
  return (
    <div className="space-y-3">
      <Card>
        <ChatGptAccount descriptionMode="inline" grouped={true} />
      </Card>
      <ul className="list-disc ms-5 space-y-1 text-sm text-text/70">
        <li>{t("onboarding.guide.chatgpt.usesAssistant")}</li>
        <li>{t("onboarding.guide.chatgpt.usesReports")}</li>
        <li>{t("onboarding.guide.chatgpt.notSpeech")}</li>
      </ul>
    </div>
  );
};

const FelixStep: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const enabled = getSetting("assistant_enabled") ?? true;
  const name = getSetting("assistant_name") ?? "Felix";
  // undefined = still checking.
  const [account, setAccount] = useState<string | null | undefined>();
  useEffect(() => {
    commands.chatgptAccount().then(setAccount);
  }, []);

  const signedIn = account !== null && account !== undefined;
  return (
    <div className="space-y-3">
      <Card>
        <ToggleSwitch
          checked={enabled}
          onChange={(v) => updateSetting("assistant_enabled", v)}
          isUpdating={isUpdating("assistant_enabled")}
          disabled={!signedIn && !enabled}
          label={t("onboarding.guide.felix.toggle", { name })}
          description={
            signedIn
              ? t("onboarding.guide.felix.privacy")
              : t("onboarding.guide.felix.needsSignIn")
          }
          descriptionMode="inline"
          grouped={true}
        />
      </Card>
      {enabled && signedIn && (
        <p className="text-sm text-text/70">
          {t("onboarding.guide.felix.tryIt", { name })}
        </p>
      )}
      <p className="text-sm text-text/60">
        {t("onboarding.guide.felix.more", { name })}
      </p>
    </div>
  );
};

export default Guide;
