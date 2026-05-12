<script lang="ts">
  import { RotateCcw, Router, Wrench } from 'lucide-svelte';
  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import { Input } from '$lib/components/ui/input';
  import { formatDateTime, formatState } from '$lib/nucleus/format';
  import { cn } from '$lib/utils';
  import type { ActionSummary, AuditEvent } from '$lib/nucleus/schemas';

  type ActionFormValues = Record<string, Record<string, string>>;

  let {
    actions,
    auditEvents,
    actionFormValues,
    actionRunningId = null,
    actionConfirmId = null,
    badgeVariantForActionRisk,
    badgeVariantForAuditStatus,
    setActionFormValue,
    onRunAction
  }: {
    actions: ActionSummary[];
    auditEvents: AuditEvent[];
    actionFormValues: ActionFormValues;
    actionRunningId?: string | null;
    actionConfirmId?: string | null;
    badgeVariantForActionRisk: (risk: string) => 'default' | 'secondary' | 'warning' | 'destructive';
    badgeVariantForAuditStatus: (status: string) => 'default' | 'secondary' | 'warning' | 'destructive';
    setActionFormValue: (actionId: string, parameterName: string, value: string) => void;
    onRunAction: (action: ActionSummary) => void;
  } = $props();
</script>

<section class="space-y-4 pt-6">
  <div class="space-y-1">
    <div class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Actions</div>
    <div class="text-sm text-zinc-400">
      Operational actions stay available, but they do not need to crowd the transcript.
    </div>
  </div>

  <div class="space-y-3">
    {#each actions as action}
      <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3">
        <div class="flex items-start justify-between gap-3">
          <div class="min-w-0">
            <div class="flex flex-wrap items-center gap-2">
              <div class="text-sm font-medium text-zinc-100">{action.title}</div>
              <Badge variant={badgeVariantForActionRisk(action.risk)}>{formatState(action.risk)}</Badge>
            </div>
            <div class="mt-1 text-xs leading-5 text-zinc-500">{action.summary}</div>
          </div>
        </div>

        {#if action.parameters.length > 0}
          <div class="mt-3 grid gap-3">
            {#each action.parameters as parameter}
              <div class="space-y-1.5">
                <div class="text-[11px] font-medium uppercase tracking-[0.14em] text-zinc-500">
                  {parameter.name}
                </div>
                <Input
                  value={actionFormValues[action.id]?.[parameter.name] ?? ''}
                  placeholder={parameter.default_value || parameter.description}
                  oninput={(event) =>
                    setActionFormValue(
                      action.id,
                      parameter.name,
                      (event.currentTarget as HTMLInputElement).value
                    )}
                />
                {#if parameter.description}
                  <div class="text-[11px] text-zinc-600">{parameter.description}</div>
                {/if}
              </div>
            {/each}
          </div>
        {/if}

        <div class="mt-3 flex flex-wrap gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={actionRunningId === action.id}
            onclick={() => onRunAction(action)}
          >
            {#if action.id === 'runtime.refresh'}
              <RotateCcw class={cn('size-4', actionRunningId === action.id && 'animate-spin')} />
            {:else if action.id === 'workspace.sync'}
              <Router class={cn('size-4', actionRunningId === action.id && 'animate-spin')} />
            {:else}
              <Wrench class="size-4" />
            {/if}
            <span>
              {actionConfirmId === action.id
                ? 'Confirm'
                : actionRunningId === action.id
                  ? 'Running'
                  : 'Run'}
            </span>
          </Button>
        </div>
      </div>
    {/each}
  </div>
</section>

<section class="space-y-4 pt-6">
  <div class="space-y-1">
    <div class="text-xs font-medium uppercase tracking-[0.16em] text-zinc-500">Recent Activity</div>
    <div class="text-sm text-zinc-400">
      Audit history stays live from the Nucleus stream, without taking over the session page.
    </div>
  </div>

  <div class="space-y-3">
    {#each auditEvents as event}
      <div class="rounded-xl border border-zinc-800 bg-zinc-900/75 px-3 py-3">
        <div class="flex items-center justify-between gap-3">
          <div class="truncate text-sm font-medium text-zinc-100">{event.summary}</div>
          <Badge variant={badgeVariantForAuditStatus(event.status)}>
            {formatState(event.status)}
          </Badge>
        </div>
        <div class="mt-2 text-xs leading-5 text-zinc-500">{event.detail}</div>
        <div class="mt-2 text-[11px] text-zinc-600">{formatDateTime(event.created_at)}</div>
      </div>
    {/each}
  </div>
</section>
