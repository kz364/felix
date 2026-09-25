import HandyHand from "./HandyHand";

/** The hand plus "Handy" in the display serif (docs/DESIGN.md). */
const Wordmark = ({ size = 22 }: { size?: number }) => (
  <span className="inline-flex items-center gap-2 text-text">
    <HandyHand width={size * 0.82} height={size * 0.88} />
    {/* eslint-disable-next-line i18next/no-literal-string */}
    <span className="font-display leading-none" style={{ fontSize: size }}>
      Handy
    </span>
  </span>
);

export default Wordmark;
