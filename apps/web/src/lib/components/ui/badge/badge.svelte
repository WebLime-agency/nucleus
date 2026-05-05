<script lang="ts">
  import { cn } from '$lib/utils';
  import type { Snippet } from 'svelte';
  import type { HTMLAttributes } from 'svelte/elements';

  interface Props extends HTMLAttributes<HTMLSpanElement> {
    variant?: 'default' | 'secondary' | 'outline' | 'warning' | 'destructive';
    children: Snippet;
  }

  let { variant = 'default', class: className, children, ...rest }: Props = $props();

  const variants: Record<string, string> = {
    default: 'bg-lime-300/12 text-lime-200',
    secondary: 'bg-zinc-900 text-zinc-300',
    outline: 'border border-zinc-800 text-zinc-300',
    warning: 'bg-amber-500/12 text-amber-200',
    destructive: 'bg-red-500/12 text-red-200'
  };
</script>

<span
  class={cn(
    'inline-flex items-center gap-1 rounded-full px-2.5 py-1 text-[11px] font-medium',
    variants[variant],
    className
  )}
  {...rest}
>
  {@render children()}
</span>
