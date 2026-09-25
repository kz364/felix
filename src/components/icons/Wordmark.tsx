import CatMark from "./CatMark";

/** The cat plus "Felix" in the display serif (docs/DESIGN.md). */
const Wordmark = ({ size = 22 }: { size?: number }) => (
  <span className="inline-flex items-center gap-2 text-text">
    <CatMark size={size * 1.05} />
    {/* eslint-disable-next-line i18next/no-literal-string */}
    <span className="font-display leading-none" style={{ fontSize: size }}>
      Felix
    </span>
  </span>
);

export default Wordmark;
