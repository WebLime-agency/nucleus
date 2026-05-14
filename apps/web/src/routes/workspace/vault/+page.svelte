<script lang="ts">
  import { onMount } from 'svelte';
  import { AlertTriangle, Clipboard, KeyRound, Lock, Plus, RefreshCw, ShieldCheck, Trash2, Unlock } from 'lucide-svelte';
  import { WorkspacePageHeader } from '$lib/components/app/workspace';
  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '$lib/components/ui/card';
  import { Input } from '$lib/components/ui/input';
  import { Label } from '$lib/components/ui/label';
  import { Select } from '$lib/components/ui/select';
  import { Textarea } from '$lib/components/ui/textarea';
  import { createVaultSecret, deleteVaultSecret, deleteVaultSecretPolicy, fetchVaultSecretPolicies, fetchVaultSecrets, fetchVaultStatus, initVault, lockVault, unlockVault, updateVaultSecret, upsertVaultSecretPolicy } from '$lib/nucleus/client';
  import type { VaultSecretPolicySummary, VaultSecretSummary, VaultStatusSummary } from '$lib/nucleus/schemas';

  let status = $state<VaultStatusSummary | null>(null);
  let secrets = $state<VaultSecretSummary[]>([]);
  let policies = $state<Record<string, VaultSecretPolicySummary[]>>({});
  let loading = $state(true);
  let saving = $state(false);
  let error = $state<string | null>(null);
  let success = $state<string | null>(null);
  let passphrase = $state('');
  let editingSecretId = $state<string | null>(null);
  let secretForm = $state({ name: '', description: '', secret: '' });
  let policySecretId = $state('');
  let policyForm = $state({ consumer_kind: 'mcp', consumer_id: '', permission: 'read', approval_mode: 'ask' });

  let initialized = $derived(Boolean(status?.initialized));
  let locked = $derived(Boolean(status?.locked));
  let canManage = $derived(initialized && !locked);

  async function loadVault() {
    loading = true;
    try {
      status = await fetchVaultStatus();
      if (status.initialized) {
        secrets = (await fetchVaultSecrets()).secrets;
        const next: Record<string, VaultSecretPolicySummary[]> = {};
        for (const secret of secrets) next[secret.id] = (await fetchVaultSecretPolicies(secret.id)).policies;
        policies = next;
      } else {
        secrets = [];
        policies = {};
      }
      error = null;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'Failed to load Vault.';
    } finally {
      loading = false;
    }
  }

  async function initialize() { saving = true; try { status = await initVault(passphrase); passphrase = ''; success = 'Workspace Vault initialized and unlocked.'; await loadVault(); } catch (cause) { error = cause instanceof Error ? cause.message : 'Failed to initialize Vault.'; } finally { saving = false; } }
  async function unlock() { saving = true; try { status = await unlockVault(passphrase); passphrase = ''; success = 'Workspace Vault unlocked.'; await loadVault(); } catch (cause) { error = cause instanceof Error ? cause.message : 'Failed to unlock Vault.'; } finally { saving = false; } }
  async function lock() { status = await lockVault(); success = 'Workspace Vault locked.'; await loadVault(); }
  function resetSecretForm() { editingSecretId = null; secretForm = { name: '', description: '', secret: '' }; }
  function editSecret(secret: VaultSecretSummary) { editingSecretId = secret.id; secretForm = { name: secret.name, description: secret.description, secret: '' }; }
  async function saveSecret() { saving = true; try { if (editingSecretId) { await updateVaultSecret(editingSecretId, { ...secretForm, scope_kind: 'workspace', scope_id: 'workspace' }); success = 'Vault secret replaced. Plaintext was not returned or displayed.'; } else { await createVaultSecret({ ...secretForm, scope_kind: 'workspace', scope_id: 'workspace' }); success = 'Vault secret created. Plaintext was not returned or displayed.'; } resetSecretForm(); await loadVault(); } catch (cause) { error = cause instanceof Error ? cause.message : 'Failed to save Vault secret.'; } finally { saving = false; } }
  async function removeSecret(secret: VaultSecretSummary) { if (!confirm(`Delete Vault secret ${secret.name}? This removes metadata and encrypted value.`)) return; await deleteVaultSecret(secret.id); success = `Deleted Vault secret ${secret.name}.`; await loadVault(); }
  function startPolicy(secret: VaultSecretSummary) { policySecretId = secret.id; policyForm = { consumer_kind: 'mcp', consumer_id: '', permission: 'read', approval_mode: 'ask' }; }
  async function savePolicy() { if (!policySecretId) return; await upsertVaultSecretPolicy(policySecretId, policyForm); success = 'Updated Vault access policy metadata.'; await loadVault(); }
  async function removePolicy(policy: VaultSecretPolicySummary) { await deleteVaultSecretPolicy(policy.secret_id, policy.id); success = 'Deleted Vault access policy metadata.'; await loadVault(); }
  async function copyReference(secret: VaultSecretSummary) { await navigator.clipboard.writeText(`vault://workspace/${secret.name}`); success = `Copied reference for ${secret.name}.`; }
  function formatTime(value?: number | null) { if (!value) return 'Never'; return new Date(value * 1000).toLocaleString(); }
  onMount(() => { void loadVault(); });
</script>

<svelte:head><title>Nucleus - Vault</title></svelte:head>

<div class="space-y-6">
  <WorkspacePageHeader title="Workspace Vault" description="Manage encrypted workspace secrets as metadata-only references. Vault values are never prompt-visible and are never revealed after submit." />
  {#if error}<div class="rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-200">{error}</div>{/if}
  {#if success}<div class="rounded-lg border border-lime-500/30 bg-lime-500/10 px-4 py-3 text-sm text-lime-100">{success}</div>{/if}
  <Card class="border-amber-500/30 bg-amber-500/10"><CardContent class="flex gap-3 pt-6 text-sm text-amber-100"><AlertTriangle class="mt-0.5 size-5 shrink-0" /><div><strong>Vault is not prompt-visible.</strong> Browser-visible Vault APIs return metadata only. There is no reveal flow; plaintext operations may be blocked on unsafe origins.</div></CardContent></Card>

  <Card><CardHeader><CardTitle class="flex items-center gap-2"><KeyRound class="size-5" /> Vault status</CardTitle><CardDescription>Initialize, unlock, or lock the local Workspace Vault.</CardDescription></CardHeader><CardContent class="space-y-4">
    <div class="flex flex-wrap items-center gap-2 text-sm text-zinc-300"><Badge variant="outline">{status?.state ?? 'loading'}</Badge><span>Initialized: {initialized ? 'yes' : 'no'}</span><span>Locked: {locked ? 'yes' : 'no'}</span>{#if status?.cipher}<span>Cipher: {status.cipher}</span>{/if}</div>
    {#if loading}<div class="text-sm text-zinc-400">Loading Vault…</div>{:else if !initialized}<div class="grid gap-3 md:max-w-xl"><Label for="init-passphrase">First-run passphrase</Label><Input id="init-passphrase" type="password" bind:value={passphrase} autocomplete="new-password" /><Button onclick={initialize} disabled={saving || !passphrase}><ShieldCheck class="mr-2 size-4" />Initialize Vault</Button></div>{:else if locked}<div class="grid gap-3 md:max-w-xl"><Label for="unlock-passphrase">Vault passphrase</Label><Input id="unlock-passphrase" type="password" bind:value={passphrase} autocomplete="current-password" /><Button onclick={unlock} disabled={saving || !passphrase}><Unlock class="mr-2 size-4" />Unlock</Button></div>{:else}<div class="flex gap-2"><Button variant="outline" onclick={() => void loadVault()}><RefreshCw class="mr-2 size-4" />Refresh</Button><Button variant="secondary" onclick={lock}><Lock class="mr-2 size-4" />Lock Vault</Button></div>{/if}
  </CardContent></Card>

  {#if initialized}
    <div class="grid gap-6 xl:grid-cols-[minmax(0,1fr)_360px]"><Card><CardHeader><CardTitle>Workspace secrets ({secrets.length})</CardTitle><CardDescription>Metadata only. Values are encrypted and never returned by this page.</CardDescription></CardHeader><CardContent class="space-y-3">
      {#if secrets.length === 0}<div class="rounded-lg border border-dashed border-zinc-800 p-6 text-sm text-zinc-400">No workspace secrets configured.</div>{/if}
      {#each secrets as secret}<div class="rounded-lg border border-zinc-800 bg-zinc-950 p-4"><div class="flex flex-wrap items-start justify-between gap-3"><div><div class="font-semibold text-zinc-100">{secret.name}</div><div class="text-sm text-zinc-400">{secret.description || 'No description'}</div><div class="mt-2 text-xs text-zinc-500">v{secret.version} · updated {formatTime(secret.updated_at)} · last used {formatTime(secret.last_used_at)}</div></div><div class="flex flex-wrap gap-2"><Button size="sm" variant="outline" onclick={() => copyReference(secret)}><Clipboard class="mr-2 size-4" />Copy reference</Button><Button size="sm" variant="outline" disabled={!canManage} onclick={() => editSecret(secret)}>Replace</Button><Button size="sm" variant="outline" disabled={!canManage} onclick={() => startPolicy(secret)}>Manage access</Button><Button size="sm" variant="destructive" disabled={!canManage} onclick={() => removeSecret(secret)}><Trash2 class="size-4" /></Button></div></div><div class="mt-3 space-y-2 text-sm"><div class="font-medium text-zinc-300">Allowed consumers</div>{#if (policies[secret.id] ?? []).length === 0}<div class="text-zinc-500">No access policies yet.</div>{/if}{#each policies[secret.id] ?? [] as policy}<div class="flex items-center justify-between rounded border border-zinc-800 px-3 py-2 text-zinc-300"><span>{policy.consumer_kind}:{policy.consumer_id} · {policy.permission} · {policy.approval_mode}</span><Button size="sm" variant="ghost" disabled={!canManage} onclick={() => removePolicy(policy)}>Remove</Button></div>{/each}</div></div>{/each}
    </CardContent></Card>
    <div class="space-y-6"><Card><CardHeader><CardTitle class="flex items-center gap-2"><Plus class="size-5" /> {editingSecretId ? 'Replace secret' : 'Create secret'}</CardTitle><CardDescription>Plaintext is sent only to the daemon for encryption and then cleared.</CardDescription></CardHeader><CardContent class="space-y-3"><Label for="secret-name">Name</Label><Input id="secret-name" bind:value={secretForm.name} disabled={!canManage} placeholder="VERCEL_TOKEN" /><Label for="secret-description">Description</Label><Textarea id="secret-description" bind:value={secretForm.description} disabled={!canManage} rows={3} /><Label for="secret-value">Secret value</Label><Textarea id="secret-value" bind:value={secretForm.secret} disabled={!canManage} rows={4} placeholder="Value is never displayed again" /><div class="flex gap-2"><Button onclick={saveSecret} disabled={!canManage || saving || !secretForm.name || !secretForm.secret}>{saving ? 'Saving…' : editingSecretId ? 'Replace secret' : 'Create secret'}</Button>{#if editingSecretId}<Button variant="outline" onclick={resetSecretForm}>Cancel</Button>{/if}</div></CardContent></Card>
    <Card><CardHeader><CardTitle>Manage access policy</CardTitle><CardDescription>Controls which future daemon consumers may request this reference. It does not reveal values.</CardDescription></CardHeader><CardContent class="space-y-3"><Label for="policy-secret">Secret</Label><Select id="policy-secret" bind:value={policySecretId} disabled={!canManage}><option value="">Select a secret</option>{#each secrets as secret}<option value={secret.id}>{secret.name}</option>{/each}</Select><Label for="consumer-kind">Consumer kind</Label><Select id="consumer-kind" bind:value={policyForm.consumer_kind} disabled={!canManage}><option value="mcp">MCP</option><option value="action">Action</option><option value="tool">Tool</option><option value="workspace">Workspace</option></Select><Label for="consumer-id">Consumer id</Label><Input id="consumer-id" bind:value={policyForm.consumer_id} disabled={!canManage} placeholder="server-or-tool-id" /><Label for="approval-mode">Approval mode</Label><Select id="approval-mode" bind:value={policyForm.approval_mode} disabled={!canManage}><option value="ask">Ask</option><option value="allow">Allow</option><option value="deny">Deny</option></Select><Button onclick={savePolicy} disabled={!canManage || !policySecretId || !policyForm.consumer_id}>Save policy</Button></CardContent></Card></div></div>
  {/if}
</div>
