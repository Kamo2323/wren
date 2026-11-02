import type { ButtonHTMLAttributes, ReactNode } from "react";

type Variant = "primary" | "secondary" | "ghost" | "danger";
type Size = "sm" | "md";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
  icon?: ReactNode;
}

export function Button({ variant = "secondary", size = "md", icon, className = "", children, type = "button", ...rest }: ButtonProps) {
  return (
    <button type={type} className={`btn btn--${variant} btn--${size} ${className}`} {...rest}>
      {icon && <span className="btn__icon">{icon}</span>}
      {children}
    </button>
  );
}

/** Square icon-only button. `label` is required: it becomes the tooltip and the accessible name. */
export function IconButton({
  label,
  children,
  tone = "default",
  className = "",
  type = "button",
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string; tone?: "default" | "danger" }) {
  return (
    <button type={type} className={`icon-btn icon-btn--${tone} ${className}`} aria-label={label} title={label} {...rest}>
      {children}
    </button>
  );
}
