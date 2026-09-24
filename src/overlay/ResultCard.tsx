import { listen } from "@tauri-apps/api/event";
import React, { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands } from "@/bindings";

interface ResultCardProps {
  text: string;
  /** Auto-close delay; 0 keeps the card until it's closed. */
  timeoutMs: number;
  /** Bumped for each new result so the countdown restarts. */
  session: number;
}

/**
 * The pill grown into a card: shown when a dictation had nowhere to go (no
 * text field focused). Shows the text with Copy and close buttons and an
 * auto-close countdown bar that pauses while the pointer is over the card.
 */
export const ResultCard: React.FC<ResultCardProps> = ({
  text,
  timeoutMs,
  session,
}) => {
  const { t } = useTranslation();
  const cardRef = useRef<HTMLDivElement>(null);
  const [remaining, setRemaining] = useState(timeoutMs);
  // Hover comes from two places: DOM events, and the backend's cursor watch
  // (the overlay panel never becomes key, so DOM hover isn't reliable).
  const [domHover, setDomHover] = useState(false);
  const [nativeHover, setNativeHover] = useState(false);
  const [copied, setCopied] = useState(false);
  const hovered = domHover || nativeHover;

  useEffect(() => {
    setRemaining(timeoutMs);
    setCopied(false);
  }, [session, timeoutMs]);

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

  // Countdown, paused while hovered.
  useEffect(() => {
    if (timeoutMs <= 0 || hovered) return;
    let last = performance.now();
    const id = window.setInterval(() => {
      const now = performance.now();
      const step = now - last;
      last = now;
      setRemaining((r) => Math.max(0, r - step));
    }, 50);
    return () => window.clearInterval(id);
  }, [timeoutMs, hovered, session]);

  useEffect(() => {
    if (timeoutMs > 0 && remaining <= 0) commands.dismissResultOverlay();
  }, [remaining, timeoutMs]);

  const copy = async () => {
    const result = await commands.copyResultText(text);
    if (result.status === "ok") {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    }
  };

  const progress = timeoutMs > 0 ? remaining / timeoutMs : 1;

  return (
    <div
      ref={cardRef}
      className={`scard result ${hovered ? "hovered" : ""}`}
      onMouseEnter={() => setDomHover(true)}
      onMouseLeave={() => setDomHover(false)}
    >
      <div className="rhead">
        <span className="rtitle">{t("overlay.result.title")}</span>
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
      <div className="rtext">{text}</div>
      <div className="rfoot">
        <span className="rhint">
          {timeoutMs > 0 && hovered ? t("overlay.result.paused") : ""}
        </span>
        <button className={`rcopy ${copied ? "done" : ""}`} onClick={copy}>
          {copied ? t("overlay.result.copied") : t("overlay.result.copy")}
        </button>
      </div>
      {timeoutMs > 0 && (
        <div className="rbar" aria-hidden="true">
          <i style={{ transform: `scaleX(${progress})` }} />
        </div>
      )}
    </div>
  );
};
