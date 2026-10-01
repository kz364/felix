import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Check } from "lucide-react";
import { commands, type LiveDraftStatus } from "@/bindings";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { Button } from "../ui/Button";
import { useSettings } from "../../hooks/useSettings";

const DRAFT_MODEL = "moonshine-streaming-tiny-Q8_0";

interface LiveDraftProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

/** A rough live draft while you talk, in a bubble by the cursor, plus what
 *  it still needs (the small model). */
export const LiveDraft: React.FC<LiveDraftProps> = ({
  descriptionMode = "tooltip",
  grouped = false,
}) => {
  const { t } = useTranslation();
  const { getSetting, refreshSettings } = useSettings();
  const enabled = getSetting("live_draft") ?? false;
  const [busy, setBusy] = useState(false);

  const toggle = async (on: boolean) => {
    setBusy(true);
    const result = await commands.setLiveDraft(on);
    setBusy(false);
    if (result.status === "error") toast.error(result.error);
    await refreshSettings();
  };

  return (
    <>
      <ToggleSwitch
        checked={enabled}
        onChange={toggle}
        isUpdating={busy}
        label={t("settings.dictation.liveDraft.label")}
        description={t("settings.dictation.liveDraft.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
      {enabled && <LiveDraftSetup className="px-4 pb-3" />}
    </>
  );
};

/** What the live draft still needs, with a button for each; nothing once
 *  it's ready (unless `showReady`). */
export const LiveDraftSetup: React.FC<{
  className?: string;
  showReady?: boolean;
}> = ({ className = "", showReady = false }) => {
  const { t } = useTranslation();
  const [status, setStatus] = useState<LiveDraftStatus | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(
    () => commands.liveDraftStatus().then(setStatus),
    [],
  );

  useEffect(() => {
    refresh();
  }, [refresh]);

  const download = async () => {
    setBusy(true);
    const result = await commands.downloadModel(DRAFT_MODEL);
    setBusy(false);
    if (result.status === "error") toast.error(result.error);
    refresh();
  };

  if (!status) return null;
  const ready = status.model_ready;
  if (ready && !showReady) return null;
  const done = (text: string) => (
    <div className="flex items-center gap-1.5 text-success">
      <Check className="h-4 w-4" />
      {text}
    </div>
  );
  return (
    <div className={`space-y-2 text-sm text-text/70 ${className}`}>
      {status.model_ready ? (
        done(t("settings.dictation.liveDraft.modelReady"))
      ) : (
        <div className="flex items-center justify-between gap-3">
          <span>{t("settings.dictation.liveDraft.needsModel")}</span>
          <Button size="sm" onClick={download} disabled={busy}>
            {busy
              ? t("settings.dictation.liveDraft.downloading")
              : t("settings.dictation.liveDraft.download")}
          </Button>
        </div>
      )}
    </div>
  );
};
