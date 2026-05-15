<script lang="ts">
  import {
    ScrollArea,
    ScrollAreaCorner,
    ScrollAreaScrollbar,
    ScrollAreaThumb,
    ScrollAreaViewport
  } from '$lib/components/ui/scroll-area';
  import { cn } from '$lib/utils';

  type Item = {
    href: string;
    label: string;
    icon: any;
    exact?: boolean;
  };

  let {
    items,
    pathname,
    label = 'Workspace navigation'
  }: { items: Item[]; pathname: string; label?: string } = $props();

  function isActive(item: Item) {
    if (item.exact) return pathname === item.href;
    return pathname === item.href || pathname.startsWith(`${item.href}/`);
  }
</script>

<ScrollArea
  class="z-10 shrink-0 border-b border-zinc-900 bg-zinc-950/95 backdrop-blur lg:h-full lg:w-52 lg:border-r lg:border-b-0 lg:bg-transparent lg:backdrop-blur-none"
>
  <ScrollAreaViewport>
    <nav
      class="flex gap-2 px-4 py-3 sm:px-6 lg:flex-col lg:gap-1 lg:px-6 lg:py-6"
      aria-label={label}
    >
      {#each items as item}
        <a
          href={item.href}
          aria-current={isActive(item) ? 'page' : undefined}
          class={cn(
            'inline-flex shrink-0 items-center gap-2 rounded-md border px-3 py-2 text-sm font-medium transition-colors lg:w-full',
            isActive(item)
              ? 'border-lime-300/30 bg-lime-300/10 text-lime-100'
              : 'border-zinc-800 bg-zinc-950 text-zinc-400 hover:bg-zinc-900 hover:text-zinc-100'
          )}
        >
          <item.icon class="size-4" />
          <span>{item.label}</span>
        </a>
      {/each}
    </nav>
  </ScrollAreaViewport>

  <ScrollAreaScrollbar orientation="horizontal">
    <ScrollAreaThumb />
  </ScrollAreaScrollbar>
  <ScrollAreaScrollbar orientation="vertical">
    <ScrollAreaThumb />
  </ScrollAreaScrollbar>
  <ScrollAreaCorner />
</ScrollArea>
