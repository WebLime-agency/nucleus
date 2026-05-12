<script lang="ts">
  import { ShieldAlert } from 'lucide-svelte';
  import { Badge } from '$lib/components/ui/badge';
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card';

  let {
    clientVersion,
    clientSurfaceVersion,
    serverSurfaceVersion,
    minimumClientVersion,
    minimumServerVersion,
    capabilityFlags
  }: {
    clientVersion: string;
    clientSurfaceVersion: string;
    serverSurfaceVersion: string;
    minimumClientVersion: string;
    minimumServerVersion: string;
    capabilityFlags: string[];
  } = $props();
</script>

<Card>
  <CardHeader>
    <CardTitle>Compatibility</CardTitle>
    <CardDescription>
      Clients should rely on explicit Nucleus compatibility metadata instead of inferring support
      from transport or decode failures.
    </CardDescription>
  </CardHeader>
  <CardContent class="space-y-3">
    <div class="grid gap-3 sm:grid-cols-2 xl:grid-cols-5">
      <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
        <div class="flex items-center gap-2 text-xs uppercase tracking-[0.16em] text-zinc-500">
          <ShieldAlert class="size-3.5" />
          <span>Client Version</span>
        </div>
        <div class="mt-2 text-sm font-medium text-zinc-50">{clientVersion}</div>
      </div>
      <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
        <div class="flex items-center gap-2 text-xs uppercase tracking-[0.16em] text-zinc-500">
          <ShieldAlert class="size-3.5" />
          <span>Client Surface</span>
        </div>
        <div class="mt-2 text-sm font-medium text-zinc-50">{clientSurfaceVersion}</div>
      </div>
      <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
        <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Server Surface</div>
        <div class="mt-2 text-sm font-medium text-zinc-50">{serverSurfaceVersion}</div>
      </div>
      <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
        <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Minimum Client</div>
        <div class="mt-2 text-sm font-medium text-zinc-50">{minimumClientVersion}</div>
      </div>
      <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
        <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Minimum Server</div>
        <div class="mt-2 text-sm font-medium text-zinc-50">{minimumServerVersion}</div>
      </div>
    </div>

    <div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
      <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">Capability Flags</div>
      {#if capabilityFlags.length}
        <div class="mt-3 flex flex-wrap gap-2">
          {#each capabilityFlags as capability}
            <Badge variant="secondary">{capability}</Badge>
          {/each}
        </div>
      {:else}
        <div class="mt-2 text-sm text-zinc-500">No capability flags were published.</div>
      {/if}
    </div>
  </CardContent>
</Card>
