<script lang="ts">
  import { renderMarkdown } from '$lib/nucleus/markdown';
  import { cn } from '$lib/utils';

  let {
    content,
    class: className = ''
  }: {
    content: string;
    class?: string;
  } = $props();

  let rendered = $derived(renderMarkdown(content));
</script>

<div class={cn('message-markdown text-sm leading-6', className)}>
  {@html rendered}
</div>

<style>
  .message-markdown :global(* + *) {
    margin-top: 0.75rem;
  }

  .message-markdown :global(p),
  .message-markdown :global(ul),
  .message-markdown :global(ol),
  .message-markdown :global(pre),
  .message-markdown :global(blockquote),
  .message-markdown :global(hr) {
    margin: 0;
  }

  .message-markdown :global(h1),
  .message-markdown :global(h2),
  .message-markdown :global(h3),
  .message-markdown :global(h4),
  .message-markdown :global(h5),
  .message-markdown :global(h6) {
    margin: 0;
    font-weight: 600;
    color: rgb(244 244 245);
    line-height: 1.35;
  }

  .message-markdown :global(h1) {
    font-size: 1.125rem;
  }

  .message-markdown :global(h2) {
    font-size: 1rem;
  }

  .message-markdown :global(h3),
  .message-markdown :global(h4),
  .message-markdown :global(h5),
  .message-markdown :global(h6) {
    font-size: 0.9375rem;
  }

  .message-markdown :global(ul),
  .message-markdown :global(ol) {
    padding-left: 1.25rem;
  }

  .message-markdown :global(li + li) {
    margin-top: 0.25rem;
  }

  .message-markdown :global(code) {
    border-radius: 0.375rem;
    background: rgb(24 24 27);
    padding: 0.12rem 0.35rem;
    font-size: 0.8125rem;
    color: rgb(212 212 216);
  }

  .message-markdown :global(pre) {
    overflow-x: auto;
    border-radius: 0.875rem;
    border: 1px solid rgb(39 39 42);
    background: rgb(9 9 11);
    padding: 0.875rem 1rem;
  }

  .message-markdown :global(pre code) {
    background: transparent;
    padding: 0;
    color: inherit;
    font-size: 0.8125rem;
    line-height: 1.55;
  }

  .message-markdown :global(blockquote) {
    border-left: 2px solid rgb(113 113 122);
    padding-left: 0.875rem;
    color: rgb(161 161 170);
  }

  .message-markdown :global(a) {
    color: rgb(190 242 100);
    text-decoration: underline;
    text-underline-offset: 0.2rem;
  }

  .message-markdown :global(strong) {
    font-weight: 700;
    color: rgb(250 250 250);
  }

  .message-markdown :global(em) {
    font-style: italic;
  }

  .message-markdown :global(hr) {
    border: 0;
    border-top: 1px solid rgb(39 39 42);
  }
</style>
