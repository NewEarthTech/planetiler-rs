import { cn } from "@planetiler-rs/ui/utils";
import { useId } from "react";

interface FieldProps {
  label: string;
  value: string;
  onChange: (value: string) => void;
  hint?: string;
  error?: string | null;
  placeholder?: string;
  inputMode?: "numeric";
  disabled?: boolean;
}

/**
 * A labelled input for the requisition options. Local to the console rather
 * than a packages/ui primitive: there is exactly one consumer, and the bounds
 * it carries are mirrors of crates/run-api's own allowlists.
 */
export function Field({
  label,
  value,
  onChange,
  hint,
  error,
  placeholder,
  inputMode,
  disabled,
}: FieldProps) {
  const id = useId();
  return (
    <div className="flex flex-col gap-1">
      <label htmlFor={id} className="text-xs text-ink-secondary">
        {label}
      </label>
      <input
        id={id}
        type="text"
        value={value}
        inputMode={inputMode}
        placeholder={placeholder}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
        className={cn(
          "h-8 rounded-lg border bg-page px-2 font-mono text-xs text-ink outline-none",
          "focus:border-ink disabled:opacity-50",
          error ? "border-danger" : "border-ring-hairline",
        )}
      />
      {error ? (
        <p className="text-xs text-danger">{error}</p>
      ) : (
        hint && <p className="text-xs text-ink-muted">{hint}</p>
      )}
    </div>
  );
}
