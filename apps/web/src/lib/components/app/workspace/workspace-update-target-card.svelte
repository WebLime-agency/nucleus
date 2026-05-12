<script lang="ts">
  import { Button } from '$lib/components/ui/button';
  import { Input } from '$lib/components/ui/input';
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card';

  let {
    managedRelease,
    releaseChannels,
    trackedChannelInput,
    trackedRefInput,
    canSaveUpdateConfig,
    savingUpdateConfig,
    onTrackedChannelInput,
    onTrackedRefInput,
    onSave
  }: {
    managedRelease: boolean;
    releaseChannels: readonly string[];
    trackedChannelInput: string;
    trackedRefInput: string;
    canSaveUpdateConfig: boolean;
    savingUpdateConfig: boolean;
    onTrackedChannelInput: (value: string) => void;
    onTrackedRefInput: (value: string) => void;
    onSave: () => void;
  } = $props();
</script>

<div class="rounded-md border border-zinc-800 bg-zinc-950/40 px-4 py-4">
  <div class="text-xs uppercase tracking-[0.16em] text-zinc-500">
    {managedRelease ? 'Tracked Release Channel' : 'Tracked Git Ref'}
  </div>

  {#if managedRelease}
    <div class="mt-3 grid gap-3 sm:grid-cols-[minmax(0,1fr)_auto]">
      <select
        class="flex h-11 w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none transition focus:border-lime-400/40"
        value={trackedChannelInput}
        aria-label="Tracked release channel"
        onchange={(event) => onTrackedChannelInput((event.currentTarget as HTMLSelectElement).value)}
      >
        {#each releaseChannels as channel}
          <option value={channel}>{channel}</option>
        {/each}
      </select>
      <Button onclick={onSave} disabled={!canSaveUpdateConfig}>
        {savingUpdateConfig ? 'Saving' : 'Save target'}
      </Button>
    </div>
    <div class="mt-3 text-xs leading-5 text-zinc-500">
      Managed installs follow release channels, not git branches. Nucleus stores the tracked
      channel separately from the currently running release and reuses it across reconnects and
      restarts.
    </div>
  {:else}
    <div class="mt-3 grid gap-3 sm:grid-cols-[minmax(0,1fr)_auto]">
      <Input
        class="h-11"
        value={trackedRefInput}
        placeholder="main"
        spellcheck="false"
        autocapitalize="off"
        aria-label="Tracked git ref"
        oninput={(event) => onTrackedRefInput((event.currentTarget as HTMLInputElement).value)}
      />
      <Button onclick={onSave} disabled={!canSaveUpdateConfig}>
        {savingUpdateConfig ? 'Saving' : 'Save target'}
      </Button>
    </div>
    <div class="mt-3 text-xs leading-5 text-zinc-500">
      Contributor installs can track an explicit ref such as <code>main</code>. Nucleus keeps this
      target separate from the live checkout so mismatch states stay visible.
    </div>
  {/if}
</div>
