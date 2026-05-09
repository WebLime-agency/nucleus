<script lang="ts">
  import SidebarItem from './sidebar-item.svelte';

  type Props = {
    navigation: Array<{
      href: string;
      label: string;
      icon: unknown;
    }>;
    pathname: string;
    isNavActive: (href: string, pathname: string) => boolean;
    openNavigation: (href: string) => void;
    hasUpdateAvailable?: boolean;
    restartRequired?: boolean;
    updateTrackLabel?: string;
    updateLastAttemptResult?: string | null;
  };

  let {
    navigation,
    pathname,
    isNavActive,
    openNavigation,
    hasUpdateAvailable = false,
    restartRequired = false,
    updateTrackLabel = '',
    updateLastAttemptResult = null
  }: Props = $props();

  let visibleNavigation = $derived(navigation.filter((item) => item.href !== '/'));
</script>

<div class="sticky bottom-0 shrink-0 border-t border-zinc-900 bg-zinc-950/95 px-3 py-2.5 backdrop-blur">
  <nav class="grid grid-flow-col auto-cols-fr gap-1.5">
    {#each visibleNavigation as item}
      <SidebarItem
        label={item.label}
        title={item.label}
        icon={item.icon}
        compact
        active={isNavActive(item.href, pathname)}
        badge={item.href === '/workspace' && (hasUpdateAvailable || restartRequired)
          ? restartRequired
            ? 'amber'
            : 'lime'
          : null}
        onclick={() => openNavigation(item.href)}
      />
    {/each}
  </nav>

  {#if restartRequired}
    <div class="mt-2 text-[11px] text-amber-300/80">Restart required to load the latest update.</div>
  {:else if hasUpdateAvailable}
    <div class="mt-2 text-[11px] text-lime-300/80">
      {updateLastAttemptResult === 'success'
        ? `Update available on ${updateTrackLabel || 'the tracked target'}.`
        : 'Last known update available.'}
    </div>
  {/if}
</div>
