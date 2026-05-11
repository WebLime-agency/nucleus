<script lang="ts">
  import { Card, CardContent, CardHeader, CardTitle } from '$lib/components/ui/card';

  let {
    value,
    label
  }: {
    value: string;
    label: string;
  } = $props();
</script>

<Card>
  <CardHeader>
    <CardTitle>{value}</CardTitle>
  </CardHeader>
  <CardContent class="text-sm text-zinc-400">{label}</CardContent>
</Card>
