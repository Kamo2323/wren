export interface ToggleProps {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string; // accessible name (the visible label usually sits next to it)
  disabled?: boolean;
}

/** On/off switch, 40 × 24. */
export function Toggle({ checked, onChange, label, disabled }: ToggleProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      className={`toggle ${checked ? "is-on" : ""}`}
      onClick={() => onChange(!checked)}
    >
      <span className="toggle__knob" />
    </button>
  );
}
