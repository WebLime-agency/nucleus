<script lang="ts">
  import { Check, ShieldAlert } from 'lucide-svelte';
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card';
</script>

<Card>
  <CardHeader>
    <CardTitle>Update Behavior</CardTitle>
    <CardDescription>
      Nucleus keeps update truth locally and serves the embedded web client that matches
      the running release.
    </CardDescription>
  </CardHeader>
  <CardContent class="space-y-3 text-sm leading-6 text-zinc-400">
    <div class="flex items-start gap-3 rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
      <Check class="mt-0.5 size-4 shrink-0 text-lime-300/80" />
      <p>
        Background checks update Nucleus-owned state and only raise an in-app toast when the latest
        successful check found a newer target.
      </p>
    </div>
    <div class="flex items-start gap-3 rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-3">
      <ShieldAlert class="mt-0.5 size-4 shrink-0 text-zinc-500" />
      <p>
        Dev checkouts may still use git-based updates. Managed releases now resolve channel
        artifacts, verify them, swap them into place, and restart onto the matching embedded web
        build instead of pulling branches directly.
      </p>
    </div>
  </CardContent>
</Card>
