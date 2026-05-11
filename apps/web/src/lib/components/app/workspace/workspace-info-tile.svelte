<script lang="ts">
  let {
    label,
    value,
    icon,
    class: className = ''
  }: {
    label: string;
    value: string;
    icon?: any;
    class?: string;
  } = $props();
</script>

<div class={`rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3 ${className}`}>
  <div class="flex items-center gap-2 text-xs uppercase tracking-[0.16em] text-zinc-500">
    {#if icon}
      <icon class="size-3.5"></icon>
    {/if}
    <span>{label}</span>
  </div>
  <div class="mt-2 text-sm font-medium text-zinc-50">{value}</div>
</div>
