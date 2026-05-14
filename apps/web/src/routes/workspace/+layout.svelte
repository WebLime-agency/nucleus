<script lang="ts">
  import { page } from '$app/state';
  import { Bot, Cpu, KeyRound, MemoryStick, PlugZap, Settings2, Wrench } from 'lucide-svelte';

  import { cn } from '$lib/utils';
  import { ScrollArea, ScrollAreaCorner, ScrollAreaScrollbar, ScrollAreaThumb, ScrollAreaViewport } from '$lib/components/ui/scroll-area';

  const tabs = [
    { href: '/workspace', label: 'Profiles', icon: Bot, exact: true },
    { href: '/workspace/skills', label: 'Skills', icon: Wrench, exact: false },
    { href: '/workspace/mcps', label: 'MCPs', icon: PlugZap, exact: false },
    { href: '/workspace/memory', label: 'Memory', icon: MemoryStick, exact: false },
    { href: '/workspace/vault', label: 'Vault', icon: KeyRound, exact: false },
    { href: '/workspace/diagnostics', label: 'Diagnostics', icon: Cpu, exact: false },
    { href: '/workspace/settings', label: 'Settings', icon: Settings2, exact: false }
  ];

  let { children } = $props();

  let pathname = $derived(page.url.pathname);

  function isActive(tab: (typeof tabs)[number]) {
    if (tab.exact) {
      return pathname === tab.href;
    }
    return pathname === tab.href || pathname.startsWith(`${tab.href}/`);
  }
</script>

<div class="flex flex-col gap-5 lg:flex-row lg:items-start lg:gap-6">
  <ScrollArea
    class="-mx-4 shrink-0 border-b border-zinc-900 sm:-mx-6 lg:sticky lg:top-0 lg:mx-0 lg:max-h-screen lg:w-44 lg:self-start lg:border-r lg:border-b-0"
  >
    <ScrollAreaViewport>
      <nav
        class="flex gap-2 px-4 pb-3 sm:px-6 lg:flex-col lg:gap-1 lg:px-0 lg:pb-0 lg:pr-4"
        aria-label="Workspace navigation"
      >
        {#each tabs as tab}
          <a
            href={tab.href}
            aria-current={isActive(tab) ? 'page' : undefined}
            class={cn(
              'inline-flex shrink-0 items-center gap-2 rounded-md border px-3 py-2 text-sm font-medium transition-colors lg:w-full',
              isActive(tab)
                ? 'border-lime-300/30 bg-lime-300/10 text-lime-100'
                : 'border-zinc-800 bg-zinc-950 text-zinc-400 hover:bg-zinc-900 hover:text-zinc-100'
            )}
          >
            <tab.icon class="size-4" />
            <span>{tab.label}</span>
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

  <div class="min-w-0 flex-1">
    {@render children()}
  </div>
</div>
