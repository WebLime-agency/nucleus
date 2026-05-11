<script lang="ts">
  type Item = {
    title: string;
    detail: string;
  };

  let {
    items,
    columns = 'md:grid-cols-2'
  }: {
    items: Item[];
    columns?: string;
  } = $props();
</script>

<div class={`grid gap-2 text-xs leading-5 text-zinc-500 ${columns}`}>
  {#each items as item (item.title)}
    <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-3 py-2">
      <span class="text-zinc-300">{item.title}</span>: {item.detail}
    </div>
  {/each}
</div>
