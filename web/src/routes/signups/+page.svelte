<script lang="ts">
	import { onMount } from 'svelte';
	import { api, all, ApiError } from '#lib/api';
	import { session } from '#lib/session.svelte';
	/** One early-access signup (`GET /api/signup-approvals`). */
	type Signup = {
		id: string;
		email: string;
		name: string;
		company: string | null;
		message: string | null;
		source: string;
		status: string;
		created_at: string;
		suggested_tenant_name: string;
	};
	type Decision = { signup: Signup; action: 'approve' | 'reject' };
	let signups = $state<Signup[]>([]),
		loading = $state(true),
		disabled = $state(false),
		error = $state(''),
		notice = $state('');
	let decision = $state<Decision | null>(null),
		tenantName = $state(''),
		deciding = $state(false);
	let dialog = $state<HTMLDialogElement>();
	$effect(() => {
		if (dialog && decision && !dialog.open) dialog.showModal();
	});
	async function load(): Promise<Signup[]> {
		loading = true;
		error = '';
		try {
			signups = await all<Signup>('/api/signup-approvals?status=pending', { activeTenant: '' });
		} catch (e) {
			// The feature ships dark: switched off, its routes do not exist.
			if (e instanceof ApiError && e.code === 'not_found') disabled = true;
			else error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
		return signups;
	}
	function open(signup: Signup, action: Decision['action']) {
		notice = '';
		decision = { signup, action };
		tenantName = signup.suggested_tenant_name;
	}
	function close() {
		decision = null;
	}
	async function confirm() {
		if (!decision) return;
		const { signup, action } = decision;
		deciding = true;
		error = '';
		try {
			if (action === 'approve') {
				const done = await api<{ tenant: { id: string; name: string } }>(
					`/api/signup-approvals/${signup.id}/approve`,
					{ tenantName: tenantName.trim() },
					{ activeTenant: '' },
				);
				notice = `Approved ${signup.email}: workspace “${done.tenant.name}” created and an invite sent.`;
			} else {
				await api(`/api/signup-approvals/${signup.id}/reject`, {}, { activeTenant: '' });
				notice = `Rejected ${signup.email}.`;
			}
			decision = null;
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			deciding = false;
		}
	}
	onMount(() => {
		// A Telegram Approve/Reject link opens that signup's dialog prefilled; nothing changes until confirmed.
		const params = new URL(window.location.href).searchParams;
		const target = params.get('signup'),
			action = params.get('action');
		void load().then((found) => {
			const signup = found.find((s) => s.id === target);
			if (signup && (action === 'approve' || action === 'reject')) open(signup, action);
			else if (target && !disabled && !error)
				error = 'That signup is no longer pending (it was approved or rejected already).';
		});
	});
</script>

<div class="page-heading">
	<div>
		<div class="eyebrow">PLATFORM</div>
		<h1>Early-access signups</h1>
		<p class="lede">Approve a request to create its workspace and invite the requester as its admin.</p>
	</div>
	<span class="pill">{signups.length} pending</span>
</div>
{#if !session.superAdmin}<div class="empty" role="status">
		<h2>Super-admin only</h2>
		<p>Only the platform super-admin can review signups.</p>
	</div>
{:else if disabled}<div class="empty" role="status">
		<h2>Early access is off</h2>
		<p>Self-signup is not enabled on this deployment (EARLY_ACCESS_SIGNUP_ENABLED).</p>
	</div>
{:else}
	{#if error}<div class="error" role="alert">{error}</div>{/if}
	{#if notice}<div class="notice" role="status">{notice}</div>{/if}
	<section class="card">
		{#if loading}<div class="empty">Loading signups…</div>{:else}<div class="table-wrap">
				<table>
					<thead><tr><th>REQUESTER</th><th>COMPANY</th><th>SOURCE</th><th>RECEIVED</th><th></th></tr></thead
					><tbody
						>{#each signups as s (s.id)}<tr
								><td
									><strong>{s.name}</strong>
									<div class="muted">{s.email}</div>
									{#if s.message}<details>
											<summary>Message</summary>
											<pre>{s.message}</pre>
										</details>{/if}</td
								><td>{s.company ?? '—'}</td><td><span class="pill">{s.source}</span></td><td
									>{new Date(s.created_at).toLocaleString()}</td
								><td class="actions"
									><button onclick={() => open(s, 'approve')}>Approve</button>
									<button class="secondary" onclick={() => open(s, 'reject')}>Reject</button></td
								></tr
							>{:else}<tr><td colspan="5" class="empty">No pending signups.</td></tr>{/each}</tbody
					>
				</table>
			</div>{/if}
	</section>
{/if}
{#if decision}<dialog
		bind:this={dialog}
		class="drawer"
		aria-label={`${decision.action === 'approve' ? 'Approve' : 'Reject'} ${decision.signup.email}`}
		oncancel={close}
	>
		<form
			onsubmit={(e) => {
				e.preventDefault();
				void confirm();
			}}
		>
			<div class="card-header">
				<div>
					<div class="eyebrow">{decision.action === 'approve' ? 'APPROVE SIGNUP' : 'REJECT SIGNUP'}</div>
					<h2>{decision.signup.email}</h2>
				</div>
				<button type="button" class="close" onclick={close}>Close ×</button>
			</div>
			<div class="card-body">
				{#if decision.action === 'approve'}
					<label for="tenant-name">Workspace name</label>
					<input id="tenant-name" bind:value={tenantName} minlength="2" maxlength="120" required />
					<p class="muted">
						Creates the workspace, invites {decision.signup.email} as its tenant admin and issues its licence.
					</p>
				{:else}
					<p>Reject the signup of {decision.signup.name}? No workspace is created and nothing is sent.</p>
				{/if}
				<button type="submit" disabled={deciding}
					>{decision.action === 'approve' ? 'Approve and create workspace' : 'Reject signup'}</button
				>
			</div>
		</form>
	</dialog>{/if}

<style>
	.actions {
		white-space: nowrap;
		text-align: right;
	}
	.actions button + button {
		margin-left: 8px;
	}
	.secondary {
		background: white;
		color: var(--teal);
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
