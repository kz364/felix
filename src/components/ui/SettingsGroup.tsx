import React from "react";

interface SettingsGroupProps {
  title?: string;
  description?: string;
  children: React.ReactNode;
}

export const SettingsGroup: React.FC<SettingsGroupProps> = ({
  title,
  description,
  children,
}) => {
  return (
    <section className="space-y-2">
      {title && (
        <div className="px-1">
          <h2 className="text-base font-medium text-text">{title}</h2>
          {description && (
            <p className="mt-0.5 text-sm leading-snug text-text/55">
              {description}
            </p>
          )}
        </div>
      )}
      <div className="bg-surface border border-stone/20 rounded-xl overflow-visible">
        <div className="divide-y divide-stone/15">{children}</div>
      </div>
    </section>
  );
};
