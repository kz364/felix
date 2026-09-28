import { listen } from "@tauri-apps/api/event";
import React, { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands, type NoticeButton } from "@/bindings";

interface ResultCardProps {
  text: string;
  /** Heading; defaults to "Nowhere to paste". */
  title?: string | null;
  /** Auto-close delay; 0 keeps the card until it's closed. */
  timeoutMs: number;
  /** Offer Copy (a dictation nobody received). */
  showCopy: boolean;
  /** A notice's buttons (Undo, Choose microphone…). */
  actions: NoticeButton[];
  /** Bumped for each new result so the countdown restarts. */
  session: number;
}

/**
 * The pill grown into a card: shown when a dictation had nowhere to go (no
 * text field focused), with Copy, or for a notice with its own buttons.
 * Has a close button and an auto-close countdown bar that pauses while the
 * pointer is over the card.
 */
export const ResultCard: React.FC<ResultCardProps> = ({
  text,
  title,
  timeoutMs,
  showCopy,
  actions,
  session,
}) => {
  const { t } = useTranslation();
  const cardRef = useRef<HTMLDivElement>(null);
  // Hover comes from two places: DOM events, and the backend's cursor watch
  // (the overlay panel never becomes key, so DOM hover isn't reliable).
  const [domHover, setDomHover] = useState(false);
  const [nativeHover, setNativeHover] = useState(false);
  const [copied, setCopied] = useState(false);
  // After Copy: the button shows a tick, then the card shrinks back to the
  // pill and closes.
  const [closing, setClosing] = useState(false);
  const closeTimer = useRef<number>();
  const hovered = domHover || nativeHover;

  useEffect(() => {
    setCopied(false);
    setClosing(false);
    return () => window.clearTimeout(closeTimer.current);
  }, [session]);

  useEffect(() => {
    const unlisten = listen<boolean>("result-hover", (e) =>
      setNativeHover(e.payload),
    );
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Fit the native window to the card so the transparent margin around it
  // doesn't swallow clicks meant for the app below.
  useLayoutEffect(() => {
    const el = cardRef.current;
    if (!el) return;
    const fit = () => commands.fitResultOverlay(Math.ceil(el.offsetHeight + 4));
    fit();
    const observer = new ResizeObserver(fit);
    observer.observe(el);
    return () => observer.disconnect();
  }, [session]);

  const copy = async () => {
    const result = await commands.copyResultText(text);
    if (result.status === "ok") {
      setCopied(true);
      window.clearTimeout(closeTimer.current);
      closeTimer.current = window.setTimeout(() => setClosing(true), 650);
    }
  };

  return (
    <div
      ref={cardRef}
      className={`scard result ${hovered ? "hovered" : ""} ${
        closing ? "closing" : ""
      }`}
      onAnimationEnd={(e) => {
        if (e.target === e.currentTarget && closing) {
          commands.dismissResultOverlay();
        }
      }}
      onMouseEnter={() => setDomHover(true)}
      onMouseLeave={() => setDomHover(false)}
    >
      <div className="rhead">
        <span className="rtitle">{title || t("overlay.result.title")}</span>
        <button
          className="sx"
          aria-label={t("overlay.result.close")}
          onClick={() => commands.dismissResultOverlay()}
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path
              d="M4 4 L12 12 M12 4 L4 12"
              stroke="currentColor"
              strokeWidth="1.6"
              strokeLinecap="round"
            />
          </svg>
        </button>
      </div>
      {text && <div className={`rtext ${showCopy ? "" : "rnote"}`}>{text}</div>}
      <div className="rfoot">
        {actions.map((action) => (
          <button
            key={action.id}
            className="raction"
            onClick={() => commands.noticeAction(action.id)}
          >
            {action.label}
          </button>
        ))}
        {showCopy && (
          <button
            className={`rcopy ${copied ? "done" : ""}`}
            onClick={copy}
            disabled={copied}
            aria-label={
              copied ? t("overlay.result.copied") : t("overlay.result.copy")
            }
          >
            {copied ? (
              <svg className="rtick" viewBox="0 0 16 16" aria-hidden="true">
                <path
                  d="M3.5 8.5 L6.5 11.5 L12.5 4.5"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
              </svg>
            ) : (
              t("overlay.result.copy")
            )}
          </button>
        )}
      </div>
      {timeoutMs > 0 && (
        <div className="rbar" aria-hidden="true">
          {/* The countdown is a CSS animation so it drains smoothly every
              frame; hovering pauses it, and its end closes the card. Keyed so
              a new result restarts it. */}
          <i
            key={`${session}-${timeoutMs}`}
            style={{
              animationDuration: `${timeoutMs}ms`,
              animationPlayState: hovered || copied ? "paused" : "running",
            }}
            onAnimationEnd={() => commands.dismissResultOverlay()}
          />
        </div>
      )}
    </div>
  );
};
