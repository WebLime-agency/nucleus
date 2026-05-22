<script lang="ts">
  import { onMount } from 'svelte';

  import { runtimeBadgeView } from '$lib/nucleus/session-ux.js';
  import { cn } from '$lib/utils';

  type RuntimeRecord = {
    state: string;
    created_at: number;
    last_resumed_at?: number | null;
  };

  let { record, nowSeconds = null }: { record: RuntimeRecord; nowSeconds?: number | null } = $props();

  let currentNow = $state(Math.floor(Date.now() / 1000));
  let badge = $derived(runtimeBadgeView(record, currentNow));

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

  function toneClass(tone: string) {
    if (tone === 'green') return 'border-lime-400/35 bg-lime-400/10 text-lime-200';
    if (tone === 'yellow') return 'border-yellow-400/35 bg-yellow-400/10 text-yellow-100';
    if (tone === 'orange') return 'border-orange-400/35 bg-orange-400/10 text-orange-100';
    return 'border-red-400/35 bg-red-400/10 text-red-100';
  }
</script>

{#if badge}
  <span
    class={cn(
      'inline-flex h-5 shrink-0 items-center rounded-md border px-1.5 text-[11px] font-medium leading-none',
      toneClass(badge.tone)
    )}
    title={badge.title}
  >
    {badge.label}
  </span>
{/if}
