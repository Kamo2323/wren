// App-wide warning when Willow or Glaido is running too (both would paste). Sits at the top of the panel.
import { IconButton } from "./Button";
import { TriangleAlert, X } from "./Icon";
import { joinNames } from "../lib/format";

export function OthersBanner({ names, onDismiss, onMore }: { names: string[]; onDismiss: () => void; onMore?: () => void }) {
  return (
    <div className="banner" role="status">
      <span className="banner__icon" aria-hidden="true">
        <TriangleAlert size={18} strokeWidth={2.1} />
      </span>
      <p className="banner__text">
        <strong>
          {joinNames(names)} {names.length === 1 ? "is" : "are"} running.
        </strong>{" "}
        If {names.length === 1 ? "it uses" : "they use"} the same keys, everything gets pasted twice. Close{" "}
        {names.length === 1 ? "it" : "them"} or turn off {names.length === 1 ? "its" : "their"} shortcut.
      </p>
      {onMore && (
        <button type="button" className="banner__more" onClick={onMore}>
          Details
        </button>
      )}
      <IconButton label="Hide this warning" onClick={onDismiss} className="banner__close">
        <X size={17} strokeWidth={2.1} />
      </IconButton>
    </div>
  );
}
