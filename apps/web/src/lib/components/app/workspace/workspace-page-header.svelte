<script lang="ts">
  import { Badge } from '$lib/components/ui/badge';
  import { cn } from '$lib/utils';

  type BadgeVariant = 'default' | 'secondary' | 'destructive' | 'outline';

  let {
    title,
    description,
    badge,
    badgeVariant = 'default',
    actions,
    class: className
  }: {
    title: string;
    description?: string;
    badge?: string | null;
    badgeVariant?: BadgeVariant;
    actions?: import('svelte').Snippet;
    class?: string;
  } = $props();
</script>

<section class={cn('flex flex-col gap-4 lg:flex-row lg:items-start lg:justify-between', className)}>
  <div>
    <div class="flex flex-wrap items-center gap-3">
      <h1 class="text-3xl font-semibold text-zinc-50">{title}</h1>
      {#if badge}
        <Badge variant={badgeVariant}>{badge}</Badge>
      {/if}
    </div>
    {#if description}
      <p class="mt-2 max-w-3xl text-sm leading-6 text-zinc-400">{description}</p>
    {/if}
  </div>

  {#if actions}
    <div class="flex flex-wrap items-center gap-2">
      {@render actions()}
    </div>
  {/if}
</section>
