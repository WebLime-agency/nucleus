<script lang="ts">
  import { Link } from 'lucide-svelte';
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card';
  import { formatState } from '$lib/nucleus/format';
  import type { SettingsSummary } from '$lib/nucleus/schemas';
  import WorkspaceStoragePathCard from './workspace-storage-path-card.svelte';

  type SecurityPosture = SettingsSummary['security'];

  let {
    localUrl,
    hostnameUrl,
    tailscaleUrl,
    webMode,
    authEnabled,
    webRoot,
    security
  }: {
    localUrl: string;
    hostnameUrl?: string | null;
    tailscaleUrl?: string | null;
    webMode: string;
    authEnabled: boolean;
    webRoot?: string | null;
    security?: SecurityPosture | null;
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

    {#if security}
      <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
        <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Security posture</div>
        <div class="mt-3 grid gap-3 sm:grid-cols-2">
          <div>
            <div class="text-xs text-zinc-500">Configured bind</div>
            <div class="mt-1 text-sm text-zinc-100">{security.configured_bind}</div>
          </div>
          <div>
            <div class="text-xs text-zinc-500">Exposure</div>
            <div class="mt-1 text-sm text-zinc-100">{formatState(security.exposure)}</div>
          </div>
          <div>
            <div class="text-xs text-zinc-500">HTTPS</div>
            <div class="mt-1 text-sm text-zinc-100">{security.https_active ? 'Active' : 'Inactive'}</div>
          </div>
          <div>
            <div class="text-xs text-zinc-500">Current origin Vault-safe</div>
            <div class={security.current_origin_vault_safe ? 'mt-1 text-sm text-emerald-300' : 'mt-1 text-sm text-amber-300'}>
              {security.current_origin_vault_safe ? 'Yes' : 'No'} — {security.current_origin_reason}
            </div>
          </div>
        </div>
        {#if security.current_origin}
          <div class="mt-3 text-xs text-zinc-500">Origin: {security.current_origin}</div>
        {/if}
        {#if security.warnings.length > 0}
          <div class="mt-3 space-y-2">
            {#each security.warnings as warning}
              <div class="rounded-md border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-sm text-amber-100">
                {warning}
              </div>
            {/each}
          </div>
        {/if}
      </div>
    {/if}
  </CardContent>
</Card>
