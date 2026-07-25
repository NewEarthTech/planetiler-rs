import { Button } from "@planetiler-rs/ui/button";

/**
 * The id token lasts 12 h and nothing else notices when it lapses: the fleet
 * poll stops on its first 401, and the SSE stream just reconnects forever
 * (lib/live.ts). This says so, and hands back a one-click way through the
 * hosted UI — deliberately not an automatic redirect, which would yank the
 * operator out of a live run with no explanation.
 */
export function SessionBanner({ onReauth }: { onReauth: () => void }) {
  return (
    <div className="mb-4 flex flex-wrap items-center gap-3 rounded-xl border border-danger/40 bg-danger/5 px-4 py-2">
      <span className="text-xs text-danger">
        Session expired — sign in again to keep dispatching.
      </span>
      <Button variant="outline" className="ml-auto" onClick={onReauth}>
        Sign in again
      </Button>
    </div>
  );
}
