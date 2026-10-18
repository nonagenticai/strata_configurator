<script lang="ts">
	import { onMount } from 'svelte';
	import { api, all, type CarebusProvision } from '$lib/api';
	import { session } from '$lib/session.svelte';
	/** One tenant (`GET /api/tenants`); archived ones are left out unless asked for. */
	type Tenant = { id: string; name: string; created_at: string; archived_at: string | null };
	let tenants = $state<Tenant[]>([]),
		loading = $state(true),
		error = $state(''),
		notice = $state('');
	let target = $state<Tenant | null>(null),
		typed = $state(''),
		offboarding = $state(false),
		failure = $state('');
	let dialog = $state<HTMLDialogElement>();
	// Create a tenant (name + first admin email) or import one that exists elsewhere under its own id.
	let newName = $state(''),
		newAdmin = $state(''),
		importId = $state(''),
		importName = $state(''),
		importAdmin = $state(''),
		saving = $state(false),
		formError = $state('');
	/** The tenant whose care-bus provisioning failed and can be retried. */
	let retryable = $state<{ id: string; name: string } | null>(null);
	function carebusText(c: CarebusProvision | undefined): string {
		if (!c) return '';
		if (c.skipped) return 'care-bus provisioning is off (skipped).';
		if (c.ok) return 'Provisioned into care-bus.';
		const failed = c.steps.find((s) => !s.ok);
		return `care-bus provisioning failed at the ${failed?.step ?? 'tenant'} step (${failed?.error_code ?? failed?.status ?? 'error'}); retry below.`;
	}
	/** The first admin field: an email invites, anything else is a Keycloak user id (`sub`). */
	function admin(v: string): Record<string, string> {
		const t = v.trim();
		return t.includes('@') ? { first_admin_email: t } : { first_admin_sub: t };
	}
	async function create() {
		saving = true;
		formError = '';
		notice = '';
		retryable = null;
		try {
			const t = await api<Tenant & { carebus?: CarebusProvision }>(
				'/api/tenants',
				{ name: newName.trim(), first_admin_email: newAdmin.trim() },
				{ activeTenant: '' },
			);
			notice = `Created “${t.name}”. ${carebusText(t.carebus)}`;
			if (t.carebus && !t.carebus.ok && !t.carebus.skipped) retryable = { id: t.id, name: t.name };
			newName = '';
			newAdmin = '';
			session.tenants = [...session.tenants, { id: t.id, name: t.name }];
			await load();
		} catch (e) {
			formError = e instanceof Error ? e.message : String(e);
		} finally {
			saving = false;
		}
	}
	async function importTenant() {
		saving = true;
		formError = '';
		notice = '';
		retryable = null;
		try {
			const out = await api<{ tenant: Tenant; imported: boolean; carebus: CarebusProvision }>(
				'/api/tenants/import',
				{ id: importId.trim(), name: importName.trim(), ...admin(importAdmin) },
				{ activeTenant: '' },
			);
			notice = `${out.imported ? 'Imported' : 'Already imported:'} “${out.tenant.name}”. ${carebusText(out.carebus)}`;
			if (!out.carebus.ok && !out.carebus.skipped) retryable = { id: out.tenant.id, name: out.tenant.name };
			if (out.imported) session.tenants = [...session.tenants, { id: out.tenant.id, name: out.tenant.name }];
			await load();
		} catch (e) {
			formError = e instanceof Error ? e.message : String(e);
		} finally {
			saving = false;
		}
	}
	async function retry() {
		if (!retryable) return;
		const t = retryable;
		formError = '';
		try {
			const c = await api<CarebusProvision>(
				`/api/tenants/${t.id}/carebus-provision`,
				{},
				{ activeTenant: '' },
			);
			notice = `“${t.name}”: ${carebusText(c)}`;
			if (c.ok) retryable = null;
		} catch (e) {
			formError = e instanceof Error ? e.message : String(e);
		}
	}
	/** The guard: the operator must type the tenant's exact name (as the API's `confirm_name` check). */
	const confirmed = $derived(target !== null && typed.trim() === target.name);
	$effect(() => {
		if (dialog && target && !dialog.open) dialog.showModal();
	});
	async function load() {
		loading = true;
		error = '';
		try {
			tenants = await all<Tenant>('/api/tenants', { activeTenant: '' });
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	}
	function open(t: Tenant) {
		notice = '';
		failure = '';
		typed = '';
		target = t;
	}
	function close() {
		target = null;
	}
	async function offboard() {
		if (!target || !confirmed) return;
		const t = target;
		offboarding = true;
		failure = '';
		try {
			await api(`/api/tenants/${t.id}/offboard`, { confirm_name: typed.trim() }, { activeTenant: '' });
			notice = `Offboarded “${t.name}”: its licence is revoked, the workspace is archived and its users are disabled.`;
			target = null;
			// The switcher must not offer (or stay on) a tenant every request would now refuse.
			session.tenants = session.tenants.filter((s) => s.id !== t.id);
			if (session.activeTenant === t.id) session.activeTenant = '';
			await load();
		} catch (e) {
			// e.g. 502 license_provisioning_failed: nothing changed, the tenant is still live; retry.
			failure = e instanceof Error ? e.message : String(e);
		} finally {
			offboarding = false;
		}
	}
	onMount(() => {
		if (session.superAdmin) void load();
		else loading = false;
	});
</script>

<div class="page-heading">
	<div>
		<div class="eyebrow">PLATFORM</div>
		<h1>Tenants</h1>
		<p class="lede">
			Every live workspace. Offboarding revokes its licence, archives it and disables its users.
		</p>
	</div>
	<span class="pill">{tenants.length} live</span>
</div>
{#if !session.superAdmin}<div class="empty" role="status">
		<h2>Super-admin only</h2>
		<p>Only the platform super-admin can manage tenants.</p>
	</div>
{:else}
	{#if error}<div class="error" role="alert">{error}</div>{/if}
	{#if notice}<div class="notice" role="status">{notice}</div>{/if}
	{#if retryable}<button onclick={retry}>Retry care-bus provision</button>{/if}
	{#if formError}<div class="error" role="alert">{formError}</div>{/if}
	<section class="card forms">
		<form
			aria-label="Create tenant"
			onsubmit={(e) => {
				e.preventDefault();
				void create();
			}}
		>
			<h2>Create tenant</h2>
			<label for="new-name">Tenant name</label>
			<input id="new-name" bind:value={newName} required />
			<label for="new-admin">First admin email</label>
			<input id="new-admin" type="email" bind:value={newAdmin} required />
			<button type="submit" disabled={saving}>Create tenant</button>
		</form>
		<form
			aria-label="Import existing tenant"
			onsubmit={(e) => {
				e.preventDefault();
				void importTenant();
			}}
		>
			<h2>Import existing tenant</h2>
			<label for="import-id">Tenant id</label>
			<input id="import-id" bind:value={importId} required />
			<label for="import-name">Tenant name</label>
			<input id="import-name" bind:value={importName} required />
			<label for="import-admin">First admin (email or user id)</label>
			<input id="import-admin" bind:value={importAdmin} required />
			<button type="submit" disabled={saving}>Import tenant</button>
		</form>
	</section>
	<section class="card">
		{#if loading}<div class="empty">Loading tenants…</div>{:else}<div class="table-wrap">
				<table>
					<thead><tr><th>TENANT</th><th>CREATED</th><th></th></tr></thead><tbody
						>{#each tenants as t (t.id)}<tr
								><td
									><strong>{t.name}</strong>
									<div class="muted">{t.id}</div></td
								><td>{new Date(t.created_at).toLocaleString()}</td><td class="actions"
									><button class="danger" onclick={() => open(t)} aria-label={`Offboard ${t.name}`}
										>Offboard tenant</button
									></td
								></tr
							>{:else}<tr><td colspan="3" class="empty">No live tenants.</td></tr>{/each}</tbody
					>
				</table>
			</div>{/if}
	</section>
{/if}
{#if target}<dialog bind:this={dialog} class="drawer" aria-label={`Offboard ${target.name}`} oncancel={close}>
		<form
			onsubmit={(e) => {
				e.preventDefault();
				void offboard();
			}}
		>
			<div class="card-header">
				<div>
					<div class="eyebrow">OFFBOARD TENANT</div>
					<h2>{target.name}</h2>
				</div>
				<button type="button" class="close" onclick={close}>Close ×</button>
			</div>
			<div class="card-body">
				<p>
					This revokes the tenant's licence, archives the workspace and disables every user in it. It cannot
					be undone. Their logins (shared with other apps) are kept.
				</p>
				{#if failure}<div class="error" role="alert">{failure}</div>{/if}
				<label for="confirm-name">Type <strong>{target.name}</strong> to confirm</label>
				<input id="confirm-name" bind:value={typed} autocomplete="off" required />
				<button type="submit" class="danger" disabled={!confirmed || offboarding}
					>{offboarding ? 'Offboarding…' : 'Offboard this tenant'}</button
				>
			</div>
		</form>
	</dialog>{/if}

<style>
	.actions {
		white-space: nowrap;
		text-align: right;
	}
	.danger {
		background: #904c30;
		border-color: #904c30;
		color: white;
	}
	.danger:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
	.forms {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
		gap: 24px;
		padding: 18px;
	}
	.notice {
		background: var(--teal-light);
		border: 1px solid var(--line);
		padding: 18px;
		border-radius: 8px;
		font-size: 13px;
		margin-bottom: 20px;
	}
	label {
		display: block;
		font-size: 12px;
		font-weight: 600;
		margin-bottom: 8px;
	}
	input {
		width: 100%;
		margin-bottom: 12px;
	}
</style>
