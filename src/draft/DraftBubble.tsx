import React, { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { BubblePlace } from "@/bindings";

/** Longest tail of the draft shown; older words scroll off the front. */
const MAX_CHARS = 160;

/** The end of the draft, cut at a word, with an ellipsis if it was cut. */
export const tail = (text: string, max = MAX_CHARS) => {
  if (text.length <= max) return text;
  const cut = text.slice(text.length - max);
  const space = cut.indexOf(" ");
  return `…${space >= 0 && space < 24 ? cut.slice(space + 1) : cut}`;
};

/** The live draft in a see-through bubble that glides to the text cursor
 *  (or rests on the recording pill). The whole window ignores the mouse. */
const DraftBubble: React.FC = () => {
  const [text, setText] = useState("");
  const [place, setPlace] = useState<BubblePlace | null>(null);

  useEffect(() => {
    const unlisten = [
      listen<string>("draft-text", (e) => setText(e.payload)),
      listen<BubblePlace>("draft-place", (e) => setPlace(e.payload)),
    ];
    return () => {
      unlisten.forEach((p) => p.then((f) => f()));
    };
  }, []);

  if (!place) return null;
  const shown = text.trim();
  return (
    <div
      className={`draft-mover ${place.instant ? "draft-instant" : ""}`}
      style={{ transform: `translate3d(${place.x}px, ${place.y}px, 0)` }}
    >
      <div
        className={`draft-anchor ${place.instant ? "draft-instant" : ""}`}
        style={{
          transform: `translate(${place.align === "center" ? "-50%" : "0"}, ${
            place.anchor === "above" ? "-100%" : "0"
          })`,
        }}
      >
        <div className={`draft-bubble ${shown ? "draft-shown" : ""}`}>
          {tail(shown)}
          <span className="draft-caret" aria-hidden="true" />
        </div>
      </div>
    </div>
  );
};

export default DraftBubble;
