import type { ReactNode } from "react";

export interface StatTileProps {
  icon: ReactNode;
  label: string;
  /** One figure with an optional unit, or several (e.g. 9 h 42 min) via `parts`. */
  value?: string;
  unit?: string;
  parts?: Array<{ value: string; unit: string }>;
  caption?: ReactNode;
  /** Shown on hover: how the number is worked out (spec §9 asks for honest, explained formulas). */
  explain?: string;
  muted?: boolean;
}

/** A big-number tile: small label, ~44 px figure with a quieter unit, one-line caption. */
export function StatTile({ icon, label, value, unit, parts, caption, explain, muted }: StatTileProps) {
  const figures = parts ?? [{ value: value ?? "", unit: unit ?? "" }];
  return (
    <div className={`stat ${muted ? "is-muted" : ""}`} title={explain}>
      <div className="stat__label">
        <span className="stat__icon">{icon}</span>
        {label}
      </div>
      <div className="stat__value num">
        {figures.map((f, i) => (
          <span key={i} className="stat__part">
            {f.value}
            {f.unit && <span className="stat__unit">{f.unit}</span>}
          </span>
        ))}
      </div>
      {caption && <div className="stat__caption">{caption}</div>}
    </div>
  );
}
