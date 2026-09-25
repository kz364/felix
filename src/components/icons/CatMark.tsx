/**
 * Felix's cat, drawn to match the app icon (sleepy black cat). Ink follows
 * the text colour so it stays visible in dark mode; the closed eyes use the
 * accent. Same shapes as the menu-bar icons in src-tauri/resources.
 */
const CatMark = ({ size = 22 }: { size?: number }) => (
  <svg
    width={size}
    height={size}
    viewBox="4 4 56 56"
    aria-hidden="true"
    xmlns="http://www.w3.org/2000/svg"
  >
    <g className="fill-text">
      <path d="M32 20C45.5 20 56 28.5 56 39.5C56 50.5 45.5 57.5 32 57.5C18.5 57.5 8 50.5 8 39.5C8 28.5 18.5 20 32 20Z" />
      <path d="M9.5 33C9.8 24 10.6 15.5 12 10.6C12.7 8.2 15 7.6 16.9 9.2C20.6 12.3 25 16.6 28.5 21.5Z M54.5 33C54.2 24 53.4 15.5 52 10.6C51.3 8.2 49 7.6 47.1 9.2C43.4 12.3 39 16.6 35.5 21.5Z" />
    </g>
    <path
      d="M16.5 39.5Q22.5 31.5 28.5 39.5 M35.5 39.5Q41.5 31.5 47.5 39.5"
      className="stroke-accent"
      fill="none"
      strokeWidth="4"
      strokeLinecap="round"
    />
    <path
      d="M29.4 44.2H34.6C35.4 44.2 35.8 45 35.3 45.6L32.8 48.1C32.4 48.5 31.6 48.5 31.2 48.1L28.7 45.6C28.2 45 28.6 44.2 29.4 44.2Z"
      className="fill-stone"
    />
  </svg>
);

export default CatMark;
