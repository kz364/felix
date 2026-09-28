/** Asks the main window to open a page (a `SECTIONS_CONFIG` id), from
 *  anywhere in it. */
export const OPEN_SECTION_EVENT = "felix-open-section";

export const openSection = (section: string) =>
  window.dispatchEvent(
    new CustomEvent(OPEN_SECTION_EVENT, { detail: section }),
  );
