<script lang="ts">
  import { cn } from '$lib/utils';

  type Item = {
    value: string;
    label: string;
    icon?: any;
  };

  let {
    items,
    value,
    onChange
  }: {
    items: Item[];
    value: string;
    onChange: (value: string) => void;
  } = $props();
</script>

<div class="inline-flex max-w-full flex-wrap rounded-lg border border-zinc-800 bg-zinc-950/70 p-1">
  {#each items as item (item.value)}
    <button
      type="button"
      class={cn(
        'inline-flex h-9 shrink-0 items-center gap-2 rounded-md px-3 text-sm transition-colors',
        value === item.value
          ? 'bg-zinc-100 text-zinc-950'
          : 'text-zinc-400 hover:bg-zinc-900 hover:text-zinc-100'
      )}
      onclick={() => onChange(item.value)}
    >
      {#if item.icon}
        <item.icon class="size-4" />
      {/if}
      {item.label}
    </button>
  {/each}
</div>
