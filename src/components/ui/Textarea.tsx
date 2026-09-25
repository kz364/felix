import React from "react";

interface TextareaProps
  extends React.TextareaHTMLAttributes<HTMLTextAreaElement> {
  variant?: "default" | "compact";
}

export const Textarea: React.FC<TextareaProps> = ({
  className = "",
  variant = "default",
  ...props
}) => {
  const baseClasses =
    "text-sm leading-relaxed bg-surface border border-stone/30 rounded-lg text-start placeholder:text-text/35 transition-[border-color,box-shadow] duration-150 hover:border-stone/50 focus:outline-none focus:border-accent focus:ring-[3px] focus:ring-accent/20 resize-y";

  const variantClasses = {
    default: "px-3 py-2 min-h-[100px]",
    compact: "px-2.5 py-1.5 min-h-[80px]",
  };

  return (
    <textarea
      className={`${baseClasses} ${variantClasses[variant]} ${className}`}
      {...props}
    />
  );
};
