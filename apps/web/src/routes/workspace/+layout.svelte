<script lang="ts">
  import { page } from '$app/state';
  import { Bot, Cpu, MemoryStick, Settings2 } from 'lucide-svelte';

  import { cn } from '$lib/utils';

  const tabs = [
    { href: '/workspace', label: 'Profiles', icon: Bot, exact: true },
    { href: '/workspace/memory', label: 'Memory', icon: MemoryStick, exact: false },
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
  <nav
    class="-mx-4 flex shrink-0 gap-2 overflow-x-auto border-b border-zinc-900 px-4 pb-3 sm:-mx-6 sm:px-6 lg:sticky lg:top-0 lg:mx-0 lg:max-h-screen lg:w-44 lg:flex-col lg:gap-1 lg:self-start lg:overflow-y-auto lg:overflow-x-visible lg:border-b-0 lg:border-r lg:border-zinc-900 lg:px-0 lg:pb-0 lg:pr-4"
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

  <div class="min-w-0 flex-1">
    {@render children()}
  </div>
</div>
