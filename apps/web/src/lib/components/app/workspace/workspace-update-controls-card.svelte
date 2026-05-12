<script lang="ts">
  import { Download, Power, RefreshCcw } from 'lucide-svelte';
  import { Button } from '$lib/components/ui/button';
  import { Card, CardContent } from '$lib/components/ui/card';
  import { formatDateTime } from '$lib/nucleus/format';

  let {
    restartRequired,
    restarting,
    dirtyWorktree,
    latestError,
    latestErrorAt,
    checking,
    applying,
    canCheck,
    canApply,
    canRestart,
    onCheck,
    onApply,
    onRestart
  }: {
    restartRequired: boolean;
    restarting: boolean;
    dirtyWorktree: boolean;
    latestError: string | null;
    latestErrorAt: number | null;
    checking: boolean;
    applying: boolean;
    canCheck: boolean;
    canApply: boolean;
    canRestart: boolean;
    onCheck: () => void;
    onApply: () => void;
    onRestart: () => void;
  } = $props();
</script>

<Card>
  <CardContent class="space-y-4 pt-6">
    {#if restartRequired}
      <div class="rounded-md border border-amber-400/30 bg-amber-400/10 px-4 py-3 text-sm text-amber-100">
        The install payload is newer than the running Nucleus process. Restart Nucleus after
        resolving the issue.
      </div>
    {/if}

    {#if restarting}
      <div class="rounded-md border border-sky-400/30 bg-sky-400/10 px-4 py-3 text-sm text-sky-100">
        Nucleus is restarting now. This page should reconnect automatically.
      </div>
    {/if}

    {#if dirtyWorktree}
      <div class="rounded-md border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">
        The working tree has local changes. Clean or commit them before applying an update.
      </div>
    {/if}

    {#if latestError}
      <div class="rounded-md border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">
        <div class="font-medium text-red-100">Latest error</div>
        <div class="mt-1">{latestError}</div>
        {#if latestErrorAt}
          <div class="mt-1 text-xs text-red-100/80">{formatDateTime(latestErrorAt)}</div>
        {/if}
      </div>
    {/if}

    <div class="flex flex-wrap items-center gap-3">
      <Button onclick={onCheck} disabled={!canCheck}>
        <RefreshCcw class={checking ? 'size-4 animate-spin' : 'size-4'} />
        {checking ? 'Checking' : 'Check for updates'}
      </Button>

      <Button variant="secondary" onclick={onApply} disabled={!canApply}>
        <Download class={applying ? 'size-4 animate-spin' : 'size-4'} />
        {applying ? 'Updating' : 'Update now'}
      </Button>

      <Button variant="secondary" onclick={onRestart} disabled={!canRestart}>
        <Power class={restarting ? 'size-4 animate-pulse' : 'size-4'} />
        {restarting ? 'Restarting' : 'Restart Nucleus'}
      </Button>
    </div>
  </CardContent>
</Card>
