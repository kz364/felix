import React from "react";

interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?:
    | "primary"
    | "primary-soft"
    | "secondary"
    | "warning"
    | "danger"
    | "danger-ghost"
    | "ghost";
  size?: "sm" | "md" | "lg";
}

export const Button: React.FC<ButtonProps> = ({
  children,
  className = "",
  variant = "primary",
  size = "md",
  ...props
}) => {
  const baseClasses =
    "inline-flex items-center justify-center gap-1.5 font-medium rounded-lg border whitespace-nowrap transition-colors duration-150 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/30 disabled:opacity-45 disabled:cursor-not-allowed cursor-pointer";

  const variantClasses = {
    primary:
      "text-background bg-text border-text hover:bg-text/85 hover:border-text/85",
    "primary-soft":
      "text-accent bg-accent/10 border-transparent hover:bg-accent/15",
    secondary:
      "text-text bg-surface border-stone/30 hover:bg-stone/10 hover:border-stone/45",
    // Secondary's neutral resting look, but hover/focus use the semantic
    // --color-warning token (theme.css) instead of the pink accent — for
    // buttons sitting on warning surfaces like SecureInputWarning
    warning:
      "text-text bg-surface border-stone/30 hover:bg-warning/10 hover:border-warning/60",
    danger:
      "text-on-accent bg-error border-error hover:bg-error/85 hover:border-error/85",
    "danger-ghost": "text-error border-transparent hover:bg-error/10",
    ghost: "text-current border-transparent hover:bg-stone/10",
  };

  const sizeClasses = {
    sm: "h-7 px-2.5 text-xs",
    md: "h-8 px-3.5 text-[13px]",
    lg: "h-9 px-4 text-sm",
  };

  return (
    <button
      className={`${baseClasses} ${variantClasses[variant]} ${sizeClasses[size]} ${className}`}
      {...props}
    >
      {children}
    </button>
  );
};
