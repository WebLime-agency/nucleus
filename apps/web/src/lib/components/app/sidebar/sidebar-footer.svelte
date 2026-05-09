<script lang="ts">
  import FolderRoot from '@lucide/svelte/icons/folder-root';
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
    workspace: {
      projects: Array<unknown>;
      root_path: string;
    } | null;
    sessionsWithProjects: number;
    formatCount: (count: number) => string;
    compactPath: (path: string) => string;
  };

  let {
    navigation,
    pathname,
    isNavActive,
    openNavigation,
    hasUpdateAvailable = false,
    restartRequired = false,
    updateTrackLabel = '',
    updateLastAttemptResult = null,
    workspace,
    sessionsWithProjects,
    formatCount,
    compactPath
  }: Props = $props();
</script>

<div class="sticky bottom-0 shrink-0 border-t border-zinc-900 bg-zinc-950/95 px-3 py-2.5 backdrop-blur">
  <nav class="grid grid-cols-4 gap-2">
    {#each navigation as item}
      <SidebarItem
        label={item.label}
        title={item.label}
        icon={item.icon}
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

  <div class="mt-3 flex items-center gap-2 text-xs text-zinc-500">
    <FolderRoot class="size-3.5 text-zinc-600" />
    <span>{workspace ? formatCount(workspace.projects.length) : '0'} projects</span>
    {#if workspace}
      <span>-</span>
      <span>{formatCount(sessionsWithProjects)} attached</span>
    {/if}
  </div>
  {#if workspace}
    <div class="mt-1 truncate text-[11px] text-zinc-600" title={workspace.root_path}>
      {compactPath(workspace.root_path)}
    </div>
  {/if}
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
