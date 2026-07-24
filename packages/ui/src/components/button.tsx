import { useRender } from "@base-ui-components/react/use-render";
import { cn } from "@planetiler-rs/ui/utils";

const VARIANT_CLASSES = {
  solid: "border border-ink bg-ink text-page hover:opacity-85",
  outline: "border border-ring-hairline bg-surface text-ink hover:bg-page",
  ghost: "border border-transparent text-ink-secondary hover:bg-surface hover:text-ink",
} as const;

const SIZE_CLASSES = {
  sm: "h-8 px-3 text-xs",
  md: "h-9 px-4 text-sm",
} as const;

export interface ButtonProps extends React.ComponentProps<"button"> {
  render?: useRender.RenderProp;
  variant?: keyof typeof VARIANT_CLASSES;
  size?: keyof typeof SIZE_CLASSES;
}

export function Button({
  variant = "outline",
  size = "sm",
  className,
  render,
  ...props
}: ButtonProps) {
  return useRender({
    defaultTagName: "button",
    render,
    props: {
      type: "button",
      className: cn(
        "inline-flex items-center justify-center gap-1.5 rounded-lg font-medium transition-colors",
        "disabled:pointer-events-none disabled:opacity-50",
        VARIANT_CLASSES[variant],
        SIZE_CLASSES[size],
        className,
      ),
      ...props,
    },
  });
}
