import type { HTMLAttributes, ReactNode } from "react";

/** A white surface with the soft Wren border. `tone="tile"` is the filled variant used for stats. */
export function Card({
  tone = "plain",
  className = "",
  children,
  ...rest
}: HTMLAttributes<HTMLDivElement> & { tone?: "plain" | "tile"; children?: ReactNode }) {
  return (
    <div className={`card card--${tone} ${className}`} {...rest}>
      {children}
    </div>
  );
}

/** Page-section heading: title on the left, optional note or actions on the right. */
export function SectionHeader({ title, note, actions }: { title: string; note?: ReactNode; actions?: ReactNode }) {
  return (
    <div className="section-head">
      <h2 className="section-head__title">{title}</h2>
      {note && <span className="section-head__note">{note}</span>}
      {actions && <div className="section-head__actions">{actions}</div>}
    </div>
  );
}
