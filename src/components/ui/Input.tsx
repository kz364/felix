import React from "react";

interface InputProps extends React.InputHTMLAttributes<HTMLInputElement> {
  variant?: "default" | "compact";
}

export const Input: React.FC<InputProps> = ({
  className = "",
  variant = "default",
  disabled,
  ...props
}) => {
  const baseClasses =
    "text-sm bg-surface border border-stone/30 rounded-lg text-start placeholder:text-text/35 transition-[border-color,box-shadow] duration-150";

  const interactiveClasses = disabled
    ? "opacity-50 cursor-not-allowed"
    : "hover:border-stone/50 focus:outline-none focus:border-accent focus:ring-[3px] focus:ring-accent/20";

  const variantClasses = {
    default: "h-9 px-3",
    compact: "h-8 px-2.5",
  } as const;

  return (
    <input
      className={`${baseClasses} ${variantClasses[variant]} ${interactiveClasses} ${className}`}
      disabled={disabled}
      {...props}
    />
  );
};
