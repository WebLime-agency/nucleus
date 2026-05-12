<script lang="ts">
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card';
  import WorkspaceStoragePathCard from './workspace-storage-path-card.svelte';

  let {
    tokenPath
  }: {
    tokenPath?: string | null;
  } = $props();
</script>

<Card>
  <CardHeader>
    <CardTitle>Access</CardTitle>
    <CardDescription>
      Nucleus owns bearer-token auth. The local token is stored outside the repository.
    </CardDescription>
  </CardHeader>
  <CardContent class="space-y-3">
    <WorkspaceStoragePathCard label="Local token path" path={tokenPath} />

    <div class="rounded-md border border-amber-400/20 bg-amber-400/10 px-4 py-3 text-sm text-amber-100">
      Retrieve the current token with <code>nucleus auth local-token</code>, then use it in the
      web UI or any future client.
    </div>
  </CardContent>
</Card>
