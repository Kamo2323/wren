// Section building blocks shared by the pages.
import type { ReactNode } from "react";

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
