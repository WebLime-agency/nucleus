<script lang="ts">
  import { browser } from '$app/environment';
  import { page } from '$app/state';
  import SessionWorkspace from '$lib/components/app/session/session-workspace.svelte';
  import OverviewPage from './overview-page.svelte';

  const requestedSessionId = $derived.by(() =>
    browser ? page.url.searchParams.get('session') ?? '' : ''
  );
</script>

{#if requestedSessionId}
  <SessionWorkspace />
{:else}
  <OverviewPage />
{/if}
