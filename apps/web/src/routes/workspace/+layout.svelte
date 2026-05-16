<script lang="ts">
  import { page } from '$app/state';
  import { Bot, Cpu, KeyRound, MemoryStick, PlugZap, ScrollText, Settings2, Wrench } from 'lucide-svelte';

  import WorkspaceSectionNav from '$lib/components/app/workspace/workspace-section-nav.svelte';

  const tabs = [
    { href: '/workspace', label: 'Profiles', icon: Bot, exact: true },
    { href: '/workspace/skills', label: 'Skills', icon: Wrench, exact: false },
    { href: '/workspace/mcps', label: 'MCPs', icon: PlugZap, exact: false },
    { href: '/workspace/memory', label: 'Memory', icon: MemoryStick, exact: false },
    { href: '/workspace/vault', label: 'Vault', icon: KeyRound, exact: false },
    { href: '/workspace/logs', label: 'Logs', icon: ScrollText, exact: false },
    { href: '/workspace/diagnostics', label: 'Diagnostics', icon: Cpu, exact: false },
    { href: '/workspace/settings', label: 'Settings', icon: Settings2, exact: false }
  ];

  let { children } = $props();

  let pathname = $derived(page.url.pathname);
</script>

<div class="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden lg:flex-row">
  <WorkspaceSectionNav items={tabs} {pathname} />

  <div class="min-h-0 min-w-0 flex-1 overflow-y-auto px-4 py-4 pb-8 sm:px-6 lg:px-8 lg:py-6">
    {@render children()}
  </div>
</div>
