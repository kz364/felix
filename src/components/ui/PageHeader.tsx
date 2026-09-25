import React from "react";

interface PageHeaderProps {
  title: string;
  description?: React.ReactNode;
  /** Controls shown to the right of the title, e.g. a master switch. */
  actions?: React.ReactNode;
}

/** The serif title and one-line intro at the top of every page. */
export const PageHeader: React.FC<PageHeaderProps> = ({
  title,
  description,
  actions,
}) => (
  <header className="flex items-end justify-between gap-4 px-1 pb-1">
    <div className="min-w-0">
      <h1 className="font-display text-[28px] leading-tight text-text">
        {title}
      </h1>
      {description && (
        <p className="mt-1 max-w-[60ch] text-sm text-text/60">{description}</p>
      )}
    </div>
    {actions && <div className="shrink-0">{actions}</div>}
  </header>
);
