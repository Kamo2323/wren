// macOS "Fix permissions": after an update that changed Wren's signature, System Settings can show Wren
// switched on while macOS still refuses it, and toggling the switch doesn't help. Resetting Wren's
// entries (no password needed) and allowing again does. Shown wherever Wren reports a missing permission.
import { useState } from "react";
import { resetMacPermissions } from "../lib/api";
import { errorText } from "../lib/meetings";
import { useToast } from "./Toast";
import { Button } from "./Button";

/** Plain-language hint that goes next to a "permission needed" message. */
export const FIX_PERMISSIONS_HINT = "Already switched on? Press Fix permissions.";

/** Runs the reset and reports it. `onDone` re-checks the permission the caller shows. */
export function useFixPermissions(onDone?: () => void) {
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  async function fix() {
    if (busy) return;
    setBusy(true);
    try {
      await resetMacPermissions();
      toast.show("Permissions reset. Allow Wren when macOS asks.", { ms: 5000 });
    } catch (e) {
      toast.show(errorText(e), { tone: "danger", ms: 7000 });
    } finally {
      setBusy(false);
      onDone?.();
    }
  }
  return { fix, busy };
}

export function FixPermissionsButton({ onDone, variant = "secondary" }: { onDone?: () => void; variant?: "secondary" | "ghost" }) {
  const { fix, busy } = useFixPermissions(onDone);
  return (
    <Button size="sm" variant={variant} disabled={busy} onClick={() => void fix()}>
      Fix permissions
    </Button>
  );
}
