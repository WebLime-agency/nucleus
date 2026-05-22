<script lang="ts">
  import { onMount } from 'svelte';

  import { reasoningActivityView } from '$lib/nucleus/session-ux.js';
  import { cn } from '$lib/utils';

  type ReasoningRecord = {
    state: string;
    last_reasoning?: string;
    last_reasoning_at?: number | null;
  };

  let { record, nowSeconds = null }: { record: ReasoningRecord; nowSeconds?: number | null } = $props();

  let currentNow = $state(Math.floor(Date.now() / 1000));
  let activity = $derived(reasoningActivityView(record, currentNow));

  $effect(() => {
    if (nowSeconds !== null) {
      currentNow = nowSeconds;
    }
  });

  onMount(() => {
    if (nowSeconds !== null) {
      return;
    }

    const interval = window.setInterval(() => {
      currentNow = Math.floor(Date.now() / 1000);
    }, 10_000);

    return () => window.clearInterval(interval);
  });
</script>

{#if activity}
  <div
    class={cn(
      'mt-1 line-clamp-2 text-xs leading-5',
      activity.stale ? 'text-amber-200' : 'text-zinc-400'
    )}
    title={activity.text}
  >
    {activity.text}
  </div>
{/if}
