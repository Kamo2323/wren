import type { ReactNode } from "react";

export interface StatTileProps {
  icon: ReactNode;
  label: string;
  value: string;
  unit?: string;
  caption?: ReactNode;
  /** Shown on hover: how the number is worked out (spec §9 asks for honest, explained formulas). */
  explain?: string;
  muted?: boolean;
}

/** A big-number tile: small label, ~44 px figure with a quieter unit, one-line caption. */
export function StatTile({ icon, label, value, unit, caption, explain, muted }: StatTileProps) {
  return (
    <div className={`stat ${muted ? "is-muted" : ""}`} title={explain}>
      <div className="stat__label">
        <span className="stat__icon">{icon}</span>
        {label}
      </div>
      <div className="stat__value num">
        {value}
        {unit && <span className="stat__unit">{unit}</span>}
      </div>
      {caption && <div className="stat__caption">{caption}</div>}
    </div>
  );
}
