<script lang="ts">
  import { cn } from '$lib/utils';
  import type { Snippet } from 'svelte';
  import type { HTMLButtonAttributes } from 'svelte/elements';

  interface Props extends HTMLButtonAttributes {
    variant?: 'default' | 'secondary' | 'ghost' | 'outline' | 'destructive';
    size?: 'default' | 'sm' | 'icon';
    children: Snippet;
  }

  let {
    variant = 'default',
    size = 'default',
    class: className,
    children,
    ...rest
  }: Props = $props();

  const variants: Record<string, string> = {
    default:
      'bg-lime-300 text-zinc-950 hover:bg-lime-200 focus-visible:ring-lime-300/50',
    secondary:
      'bg-zinc-900 text-zinc-100 hover:bg-zinc-800 focus-visible:ring-zinc-700',
    ghost:
      'text-zinc-300 hover:bg-zinc-900 hover:text-zinc-50 focus-visible:ring-zinc-700',
    outline:
      'border border-zinc-800 bg-transparent text-zinc-200 hover:bg-zinc-900 focus-visible:ring-zinc-700',
    destructive:
      'bg-red-500/15 text-red-200 hover:bg-red-500/25 focus-visible:ring-red-500/40'
  };

  const sizes: Record<string, string> = {
    default: 'h-10 px-4 py-2 text-sm',
    sm: 'h-8 px-3 text-xs',
    icon: 'h-9 w-9'
  };
</script>

<button
  class={cn(
    'inline-flex items-center justify-center gap-2 rounded-md font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 disabled:pointer-events-none disabled:opacity-50',
    variants[variant],
    sizes[size],
    className
  )}
  {...rest}
>
  {@render children()}
</button>
