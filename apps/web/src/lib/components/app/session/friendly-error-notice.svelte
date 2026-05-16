<script lang="ts">
  import { goto } from '$app/navigation';
  import { PanelRightOpen, RotateCcw, Settings2, XCircle } from 'lucide-svelte';

  import { Button } from '$lib/components/ui/button';
  import type { UserFacingErrorSummary } from '$lib/nucleus/schemas';
  import { cn } from '$lib/utils';

  interface Props {
    userError: UserFacingErrorSummary;
    class?: string;
    onRetryJob?: () => void;
    onCancelJob?: () => void;
    onOpenJobDetails?: () => void;
    retryDisabled?: boolean;
    cancelDisabled?: boolean;
  }

  let {
    userError,
    class: className,
    onRetryJob,
    onCancelJob,
    onOpenJobDetails,
    retryDisabled = false,
    cancelDisabled = false
  }: Props = $props();

  function hasAction(action: string) {
    return userError.actions.includes(action);
  }
</script>

<div class={cn('rounded-lg border border-red-500/25 bg-red-500/10 px-3 py-3 text-sm text-red-100', className)}>
  <div class="font-medium text-red-50">{userError.title}</div>
  <div class="mt-1 text-xs leading-5 text-red-100/80">{userError.message}</div>

  <div class="mt-3 flex flex-wrap gap-2">
    {#if hasAction('open_profiles')}
      <Button variant="secondary" size="sm" onclick={() => void goto('/workspace')}>
        <Settings2 class="size-4" />
        <span>Open Profiles</span>
      </Button>
    {/if}

    {#if hasAction('retry_job') && onRetryJob}
      <Button variant="secondary" size="sm" disabled={retryDisabled} onclick={onRetryJob}>
        <RotateCcw class={cn('size-4', retryDisabled && 'animate-spin')} />
        <span>Retry Job</span>
      </Button>
    {/if}

    {#if hasAction('cancel_job') && onCancelJob}
      <Button variant="outline" size="sm" disabled={cancelDisabled} onclick={onCancelJob}>
        <XCircle class="size-4" />
        <span>Cancel Job</span>
      </Button>
    {/if}

    {#if hasAction('open_job_details') && onOpenJobDetails}
      <Button variant="ghost" size="sm" onclick={onOpenJobDetails}>
        <PanelRightOpen class="size-4" />
        <span>Open Job Details</span>
      </Button>
    {/if}
  </div>

  {#if userError.technical_detail}
    <details class="mt-3 text-xs leading-5 text-red-100/70">
      <summary class="cursor-pointer select-none text-red-100/85">Technical details</summary>
      <pre class="mt-2 max-h-40 overflow-auto whitespace-pre-wrap rounded-lg bg-zinc-950/80 px-3 py-2 text-[11px] leading-5 text-red-100/70">{userError.technical_detail}</pre>
    </details>
  {/if}
</div>
