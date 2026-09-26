import { listen } from "@tauri-apps/api/event";
import React, { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands } from "@/bindings";
import type { AgentAnswer, AgentCard as Card } from "@/bindings";

/**
 * Felix working on a computer task: the task, the last few steps and the
 * current one in plain words, questions (which apps it may use, confirming
 * anything that sends or deletes) and how it ended. Stays up while it works;
 * closing it stops the task.
 */
export const AgentCard: React.FC<{ card: Card }> = ({ card }) => {
  const { t } = useTranslation();
  const cardRef = useRef<HTMLDivElement>(null);
  const [domHover, setDomHover] = useState(false);
  const [nativeHover, setNativeHover] = useState(false);
  const hovered = domHover || nativeHover;
  const working = card.status === "working";

  useEffect(() => {
    const unlisten = listen<boolean>("result-hover", (e) =>
      setNativeHover(e.payload),
    );
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Fit the native window to the card as steps come and go.
  useLayoutEffect(() => {
    const el = cardRef.current;
    if (!el) return;
    const fit = () => commands.fitResultOverlay(Math.ceil(el.offsetHeight + 4));
    fit();
    const observer = new ResizeObserver(fit);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const answer = (a: AgentAnswer) => commands.answerAgentQuestion(a);
  const heading = working
    ? t("overlay.agent.working", { name: card.name })
    : t(`overlay.agent.status.${card.status}`, { name: card.name });

  return (
    <div
      ref={cardRef}
      className={`scard result agent ${hovered ? "hovered" : ""}`}
      onMouseEnter={() => setDomHover(true)}
      onMouseLeave={() => setDomHover(false)}
    >
      <div className="rhead">
        <span className={`rtitle agent-${card.status}`}>{heading}</span>
        <button
          className="sx"
          aria-label={
            working ? t("overlay.agent.stop") : t("overlay.result.close")
          }
          title={working ? t("overlay.agent.stop") : t("overlay.result.close")}
          onClick={() => commands.closeAgentCard()}
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
      <div className="atask">{card.task}</div>

      {(card.steps.length > 0 || card.current) && (
        <ol className="asteps">
          {card.steps.map((s, i) => (
            <li key={`${i}-${s}`} className="adone">
              <svg viewBox="0 0 16 16" aria-hidden="true">
                <path
                  d="M3.5 8.5 L6.5 11.5 L12.5 4.5"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.8"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
              </svg>
              <span>{s}</span>
            </li>
          ))}
          {working && card.current && (
            <li className="anow">
              <span className="aspin" aria-hidden="true" />
              <span>{card.current}</span>
            </li>
          )}
        </ol>
      )}

      {card.question?.kind === "app_access" && (
        <div className="aask">
          <p>
            {t("overlay.agent.appAccess", {
              name: card.name,
              app: card.question.app,
            })}
          </p>
          <div className="abtns">
            <button className="abtn" onClick={() => answer("always_deny")}>
              {t("overlay.agent.alwaysDeny")}
            </button>
            <button className="abtn" onClick={() => answer("deny")}>
              {t("overlay.agent.deny")}
            </button>
            <button className="abtn" onClick={() => answer("allow_once")}>
              {t("overlay.agent.allowOnce")}
            </button>
            {card.question.can_always && (
              <button
                className="abtn primary"
                onClick={() => answer("always_allow")}
              >
                {t("overlay.agent.alwaysAllow")}
              </button>
            )}
          </div>
          <p className="ahint">
            {card.question.can_always
              ? t("overlay.agent.sayAppAccess")
              : t("overlay.agent.sayAppAccessOnce")}
          </p>
        </div>
      )}

      {card.question?.kind === "confirm" && (
        <div className="aask">
          <p>{t("overlay.agent.confirm", { action: card.question.action })}</p>
          <div className="abtns">
            <button className="abtn" onClick={() => answer("cancel")}>
              {t("overlay.agent.cancel")}
            </button>
            <button className="abtn primary" onClick={() => answer("confirm")}>
              {t("overlay.agent.doIt")}
            </button>
          </div>
          <p className="ahint">{t("overlay.agent.sayConfirm")}</p>
        </div>
      )}

      {!working && card.message && <div className="rtext">{card.message}</div>}

      {!working && card.timeout_ms > 0 && (
        <div className="rbar" aria-hidden="true">
          <i
            key={`${card.task}-${card.status}`}
            style={{
              animationDuration: `${card.timeout_ms}ms`,
              animationPlayState: hovered ? "paused" : "running",
            }}
            onAnimationEnd={() => commands.closeAgentCard()}
          />
        </div>
      )}
    </div>
  );
};
