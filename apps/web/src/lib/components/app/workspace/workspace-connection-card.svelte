<script lang="ts">
  import { Link } from 'lucide-svelte';
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card';
  import { formatState } from '$lib/nucleus/format';
  import WorkspaceStoragePathCard from './workspace-storage-path-card.svelte';

  let {
    localUrl,
    hostnameUrl,
    tailscaleUrl,
    webMode,
    authEnabled,
    webRoot
  }: {
    localUrl: string;
    hostnameUrl?: string | null;
    tailscaleUrl?: string | null;
    webMode: string;
    authEnabled: boolean;
    webRoot?: string | null;
  } = $props();
</script>

<Card>
  <CardHeader>
    <CardTitle>Connection</CardTitle>
    <CardDescription>
      These are the Nucleus URLs for this instance and the current web delivery mode.
    </CardDescription>
  </CardHeader>
  <CardContent class="space-y-3">
    <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
      <div class="flex items-center gap-2 text-xs uppercase tracking-[0.16em] text-zinc-500">
        <Link class="size-3.5" />
        <span>Local</span>
      </div>
      <div class="mt-2 text-sm text-zinc-100">{localUrl}</div>
    </div>

    {#if hostnameUrl}
      <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
        <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Host</div>
        <div class="mt-2 text-sm text-zinc-100">{hostnameUrl}</div>
      </div>
    {/if}

    {#if tailscaleUrl}
      <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
        <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Tailscale</div>
        <div class="mt-2 text-sm text-zinc-100">{tailscaleUrl}</div>
      </div>
    {/if}

    <div class="grid gap-3 sm:grid-cols-2">
      <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
        <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Web mode</div>
        <div class="mt-2 text-sm font-medium text-zinc-50">{formatState(webMode)}</div>
      </div>
      <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
        <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Auth</div>
        <div class="mt-2 text-sm font-medium text-zinc-50">
          {authEnabled ? 'Bearer token required' : 'Disabled'}
        </div>
      </div>
    </div>

    {#if webRoot}
      <WorkspaceStoragePathCard label="Web build path" path={webRoot} />
    {/if}
  </CardContent>
</Card>
