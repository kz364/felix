import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { toast } from "sonner";
import { commands, type Proposal } from "@/bindings";
import { Button } from "../../ui/Button";
import { Textarea } from "../../ui/Textarea";

/** Describe a mistranscription; a remote model proposes a rules-file fix. */
export const ReportMistake: React.FC<{ onChange?: () => void }> = ({
  onChange,
}) => {
  const { t } = useTranslation();
  const [report, setReport] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [proposal, setProposal] = useState<Proposal | null>(null);
  const [saved, setSaved] = useState(false);

  const propose = async () => {
    setBusy(true);
    setError(null);
    setProposal(null);
    setSaved(false);
    const result = await commands.proposeRules(report);
    setBusy(false);
    if (result.status === "ok") setProposal(result.data);
    else setError(result.error);
    onChange?.();
  };

  const save = async () => {
    if (!proposal) return;
    const result = await commands.saveRules(proposal.rules, proposal.id);
    if (result.status === "ok") {
      setSaved(true);
      setProposal(null);
      setReport("");
      onChange?.();
    } else setError(result.error);
  };

  const undo = async () => {
    const result = await commands.undoRules();
    if (result.status === "ok") {
      setSaved(false);
      onChange?.();
    } else setError(result.error);
  };

  const changed = proposal?.diff.some((l) => l.kind !== "same") ?? false;
  const failing = proposal?.tests.filter((r) => !r.passed).length ?? 0;

  // A copy for another app: JSON for most, or the TOML file itself.
  const exportRules = async () => {
    const path = await saveDialog({
      defaultPath: "felix-rules.json",
      filters: [
        { name: "JSON", extensions: ["json"] },
        { name: "TOML", extensions: ["toml"] },
      ],
    });
    if (!path) return;
    const result = await commands.exportRules(path);
    if (result.status === "error") toast.error(result.error);
    else toast.success(t("settings.vocabulary.report.exported"));
  };

  return (
    <div className="px-4 py-3">
      <div className="space-y-3">
        <Textarea
          value={report}
          onChange={(e) => setReport(e.target.value)}
          placeholder={t("settings.vocabulary.report.placeholder")}
          className="w-full"
          variant="compact"
        />
        <div className="flex gap-2">
          <Button size="sm" onClick={propose} disabled={busy || !report.trim()}>
            {busy
              ? t("settings.vocabulary.report.working")
              : t("settings.vocabulary.report.fix")}
          </Button>
          <Button
            size="sm"
            variant="secondary"
            onClick={() => commands.openRulesFile()}
          >
            {t("settings.vocabulary.report.openFile")}
          </Button>
          <Button size="sm" variant="secondary" onClick={exportRules}>
            {t("settings.vocabulary.report.export")}
          </Button>
        </div>
        {error && <p className="text-sm text-error">{error}</p>}
        {saved && (
          <div className="flex items-center gap-2 text-sm text-text/70">
            <span>{t("settings.vocabulary.report.saved")}</span>
            <Button size="sm" variant="ghost" onClick={undo}>
              {t("settings.vocabulary.report.undo")}
            </Button>
          </div>
        )}
        {proposal && (
          <div className="space-y-3 rounded-lg border border-stone/30 p-3">
            <p className="text-sm leading-relaxed">{proposal.explanation}</p>
            {proposal.needs_code_change && (
              <p className="text-sm text-warning">
                {t("settings.vocabulary.report.needsCode")}
              </p>
            )}
            {proposal.error && (
              <p className="text-sm text-error">
                {t("settings.vocabulary.report.invalid", {
                  error: proposal.error,
                })}
              </p>
            )}
            {changed && (
              <pre className="max-h-64 overflow-auto rounded-md bg-stone/10 p-2 text-xs leading-snug">
                {proposal.diff
                  .filter((l) => l.kind !== "same")
                  .map((l, i) => (
                    <div
                      key={i}
                      className={
                        l.kind === "added" ? "text-success" : "text-error"
                      }
                    >
                      {(l.kind === "added" ? "+ " : "- ") + l.text}
                    </div>
                  ))}
              </pre>
            )}
            {proposal.tests.length > 0 && (
              <ul className="space-y-1 text-xs">
                {proposal.tests.map((r, i) => (
                  <li key={i}>
                    {t("settings.vocabulary.report.test", {
                      mark: r.passed ? "✓" : "✗",
                      said: r.said,
                      got: r.got,
                    })}
                    {!r.passed &&
                      t("settings.vocabulary.report.expected", {
                        expect: r.expect,
                      })}
                    {r.model_decides &&
                      t("settings.vocabulary.report.modelDecides")}
                  </li>
                ))}
              </ul>
            )}
            <div className="flex gap-2">
              <Button
                size="sm"
                onClick={save}
                disabled={!changed || !!proposal.error}
              >
                {failing > 0
                  ? t("settings.vocabulary.report.applyAnyway")
                  : t("settings.vocabulary.report.apply")}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                onClick={() => setProposal(null)}
              >
                {t("settings.vocabulary.report.discard")}
              </Button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
};
