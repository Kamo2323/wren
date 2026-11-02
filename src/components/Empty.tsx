// Empty states for every list: the Ctrl + Win keycaps, or an icon in a soft disc, then a title and a line.
import type { ReactNode } from "react";

/** The dictation chord drawn as two keycaps. */
export function ChordKeys() {
  return (
    <div className="empty__keys" aria-hidden="true">
      <kbd className="key">Ctrl</kbd>
      <span className="empty__plus">+</span>
      <kbd className="key">
        <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
          <rect x="1" y="1" width="6.5" height="6.5" rx="1" />
          <rect x="8.5" y="1" width="6.5" height="6.5" rx="1" />
          <rect x="1" y="8.5" width="6.5" height="6.5" rx="1" />
          <rect x="8.5" y="8.5" width="6.5" height="6.5" rx="1" />
        </svg>
        Win
      </kbd>
    </div>
  );
}

export function EmptyState({
  art,
  icon,
  title,
  children,
  note,
  action,
  compact,
}: {
  art?: ReactNode;
  icon?: ReactNode;
  title: string;
  children?: ReactNode;
  note?: ReactNode;
  action?: ReactNode;
  compact?: boolean;
}) {
  return (
    <div className={`empty ${compact ? "empty--compact" : ""}`}>
      {art}
      {icon && (
        <div className="empty__icon" aria-hidden="true">
          {icon}
        </div>
      )}
      <h3 className="empty__title">{title}</h3>
      {children && <p className="empty__text">{children}</p>}
      {action && <div className="empty__action">{action}</div>}
      {note && <p className="empty__note">{note}</p>}
    </div>
  );
}
