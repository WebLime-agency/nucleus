<script lang="ts">
  import { Send } from 'lucide-svelte';
  import { Button } from '$lib/components/ui/button';
  import { Textarea } from '$lib/components/ui/textarea';
  import { cn } from '$lib/utils';

  let {
    promptText = $bindable(''),
    composerTextareaElement = $bindable<HTMLTextAreaElement | null>(null),
    composerHint,
    sending = false,
    promptReady = false,
    disabled = false,
    onComposerKeydown,
    onComposerPaste,
    onSubmit
  }: {
    promptText: string;
    composerTextareaElement: HTMLTextAreaElement | null;
    composerHint: string;
    sending: boolean;
    promptReady: boolean;
    disabled: boolean;
    onComposerKeydown: (event: KeyboardEvent) => void;
    onComposerPaste: (event: ClipboardEvent) => void;
    onSubmit: () => void;
  } = $props();
</script>

<div class="flex items-end gap-2">
  <Textarea
    bind:ref={composerTextareaElement}
    bind:value={promptText}
    rows={1}
    class="max-h-[10.5rem] min-h-10 flex-1 resize-none border-0 bg-transparent px-1 py-2 text-sm leading-5 text-zinc-100 focus:border-transparent focus-visible:ring-0"
    placeholder="Send a message..."
    spellcheck={false}
    aria-describedby="composer-hint"
    {disabled}
    onkeydown={onComposerKeydown}
    onpaste={onComposerPaste}
  ></Textarea>

  <Button
    variant="default"
    size="icon"
    aria-label={sending ? 'Sending prompt' : 'Send prompt'}
    disabled={!promptReady || disabled || sending}
    onclick={onSubmit}
  >
    <Send class={cn('size-4', sending && 'animate-pulse')} />
  </Button>
</div>

<div id="composer-hint" class="sr-only">
  {composerHint} Press Enter to send. Press Shift and Enter to add a new line.
</div>
