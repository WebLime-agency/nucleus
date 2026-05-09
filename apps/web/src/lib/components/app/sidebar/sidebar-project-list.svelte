<script lang="ts">
  import type { ProjectSummary } from '$lib/nucleus/schemas';
  import SidebarProjectItem from './sidebar-project-item.svelte';

  type Props = {
    projects: ProjectSummary[];
    selectedProjectId?: string;
    onSelect: (projectId: string) => void;
  };

  let { projects, selectedProjectId = '', onSelect }: Props = $props();
</script>

<div class="px-3 py-3">
  <div class="space-y-2">
    <SidebarProjectItem
      title="Workspace scratch"
      description="Start without an attached project."
      selected={selectedProjectId === ''}
      onclick={() => onSelect('')}
    />

    {#each projects as project}
      <SidebarProjectItem
        title={project.title}
        description={project.relative_path}
        selected={project.id === selectedProjectId}
        onclick={() => onSelect(project.id)}
      />
    {/each}
  </div>
</div>
