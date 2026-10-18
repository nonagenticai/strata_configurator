<script lang="ts">
	import {
		api,
		all,
		ApiError,
		type Conflict,
		type Drift,
		type PushStep,
		type Release,
		type Target,
		type Violation,
	} from '$lib/api';
	import { session } from '$lib/session.svelte';
	let targets = $state<Target[]>([]),
		selected = $state<Target | null>(null),
		releases = $state<Release[]>([]),
		shown = $state<Release | null>(null),
		steps = $state<PushStep[]>([]),
		conflicts = $state<Conflict[]>([]),
		drift = $state<Drift | null>(null);
	let loading = $state(true),
		busy = $state(false),
		error = $state(''),
		violations = $state<Violation[]>([]);
	let job = $state('customer_care_agent'),
		industry = $state('retail'),
		departments = $state('faq, logistics, order-support, payments');
	/** Building and pushing need tenant_admin (a super-admin acting in the tenant counts). */
	const canWrite = $derived(session.superAdmin || session.workspace?.role === 'tenant_admin');

	function fail(e: unknown) {
		error = e instanceof ApiError ? `${e.code}: ${e.message}` : e instanceof Error ? e.message : String(e);
		violations =
			e instanceof ApiError && (e.code === 'bundle_incomplete' || e.code === 'bundle_invalid')
				? (e.errors as unknown as Violation[])
				: [];
	}
	function reset() {
		error = '';
		violations = [];
	}
	/** The conflicts care-bus reported with the newest successful release step. */
	function conflictsOf(list: PushStep[]): Conflict[] {
		const last = [...list].reverse().find((s) => s.step === 'release' && s.ok);
		return ((last?.response?.conflicts as Conflict[] | undefined) ?? []).slice();
	}
	async function load() {
		loading = true;
		reset();
		try {
			targets = (await api<{ items: Target[] }>('/api/targets')).items;
			const first = targets[0];
			if (first) await select(first);
		} catch (e) {
			fail(e);
		} finally {
			loading = false;
		}
	}
	async function select(t: Target) {
		selected = t;
		shown = null;
		steps = [];
		conflicts = [];
		drift = null;
		releases = await all<Release>(`/api/targets/${t.id}/releases`);
	}
	async function createTarget() {
		reset();
		busy = true;
		try {
			const selection: Record<string, unknown> = {
				departments: departments
					.split(',')
					.map((d) => d.trim())
					.filter(Boolean),
			};
			if (industry.trim()) selection.industry = industry.trim();
			const t = await api<Target>('/api/targets', { job: job.trim(), selection });
			targets = [t, ...targets];
			await select(t);
		} catch (e) {
			fail(e);
		} finally {
			busy = false;
		}
	}
	async function build() {
		if (!selected) return;
		reset();
		busy = true;
		try {
			await api<Release>(`/api/targets/${selected.id}/releases`, {});
			await select(selected);
		} catch (e) {
			fail(e);
		} finally {
			busy = false;
		}
	}
	async function show(r: Release) {
		reset();
		try {
			const out = await api<{ release: Release; push_steps: PushStep[] }>(`/api/releases/${r.id}`);
			shown = out.release;
			steps = out.push_steps;
			conflicts = conflictsOf(steps);
		} catch (e) {
			fail(e);
		}
	}
	async function push(r: Release) {
		reset();
		busy = true;
		shown = r;
		try {
			const out = await api<{ steps: PushStep[]; carebus: { conflicts?: Conflict[] } | null }>(
				`/api/releases/${r.id}/push`,
				{},
			);
			steps = out.steps;
			conflicts = out.carebus?.conflicts ?? [];
		} catch (e) {
			fail(e);
			// The steps are recorded even when care-bus refused: show them.
			try {
				steps = (await api<{ push_steps: PushStep[] }>(`/api/releases/${r.id}`)).push_steps;
			} catch {
				/* the error above is the one to show */
			}
		} finally {
			busy = false;
		}
	}
	async function checkDrift() {
		if (!selected) return;
		reset();
		try {
			drift = await api<Drift>(`/api/targets/${selected.id}/drift`);
		} catch (e) {
			fail(e);
		}
	}
	$effect(() => {
		void session.activeTenant;
		void load();
	});
</script>

<div class="page-heading">
	<div>
		<div class="eyebrow">WORKSPACE</div>
		<h1>Releases</h1>
		<p class="lede">Build a versioned base for a job and push it to care-bus.</p>
	</div>
	<span class="pill">{targets.length} targets</span>
</div>
{#if error}<div class="error" role="alert">
		{error}
		{#if violations.length}<ul>
				{#each violations as v (v.path + v.message)}<li><code>{v.path}</code> {v.message}</li>{/each}
			</ul>{/if}
	</div>{/if}
<section class="card">
	<div class="card-header"><h2>Targets</h2></div>
	<div class="card-body">
		{#if loading}<div class="empty">Loading targets…</div>
		{:else}
			<ul class="targets">
				{#each targets as t (t.id)}<li>
						<button class:active={selected?.id === t.id} onclick={() => select(t)}>{t.job}</button>
					</li>{:else}<li class="empty">No targets yet.</li>{/each}
			</ul>
		{/if}
		{#if canWrite}<form
				class="new-target"
				onsubmit={(e) => {
					e.preventDefault();
					void createTarget();
				}}
			>
				<label for="target-job">Job</label>
				<input id="target-job" bind:value={job} required />
				<label for="target-industry">Industry</label>
				<input id="target-industry" bind:value={industry} />
				<label for="target-departments">Departments (comma separated)</label>
				<input id="target-departments" bind:value={departments} />
				<button type="submit" disabled={busy}>Create target</button>
			</form>{/if}
	</div>
</section>
{#if selected}<section class="card">
		<div class="card-header">
			<h2>{selected.job}</h2>
			<div class="actions">
				{#if canWrite}<button onclick={build} disabled={busy}>Build release</button>{/if}
				<button onclick={checkDrift}>Check drift</button>
			</div>
		</div>
		<div class="card-body">
			{#if drift}<div class="drift">
					<span class="badge" data-testid="drift-status" data-status={drift.status}>{drift.status}</span>
					{#if drift.config_changed}<span class="muted">configuration changed since the last build</span>{/if}
					{#if drift.carebus}<span class="muted">care-bus holds {drift.carebus.release_id}</span>{/if}
				</div>{/if}
			<div class="table-wrap">
				<table>
					<thead><tr><th>RELEASE</th><th>HASH</th><th>BUILT</th><th></th></tr></thead>
					<tbody
						>{#each releases as r (r.id)}<tr>
								<td><button class="text-button" onclick={() => show(r)}>{r.release_id}</button></td>
								<td><code>{r.hash.slice(0, 19)}…</code></td>
								<td>{new Date(r.created_at).toLocaleString()}</td>
								<td class="actions"
									>{#if canWrite}<button
											onclick={() => push(r)}
											disabled={busy}
											aria-label={`Push ${r.release_id}`}>Push</button
										>{/if}</td
								>
							</tr>{:else}<tr><td colspan="4" class="empty">No releases built yet.</td></tr>{/each}</tbody
					>
				</table>
			</div>
		</div>
	</section>{/if}
{#if shown}<section class="card">
		<div class="card-header"><h2>{shown.release_id}: push steps</h2></div>
		<div class="card-body">
			<table>
				<thead><tr><th>STEP</th><th>STATUS</th><th>RESULT</th></tr></thead>
				<tbody
					>{#each steps as s (s.id)}<tr>
							<td>{s.step}</td><td>{s.status ?? '—'}</td><td>{s.ok ? 'ok' : (s.error_code ?? 'failed')}</td>
						</tr>{:else}<tr><td colspan="3" class="empty">Not pushed yet.</td></tr>{/each}</tbody
				>
			</table>
			<h3>Conflicts</h3>
			<table aria-label="Conflicts">
				<thead><tr><th>KIND</th><th>OBJECT</th><th>KEY</th><th>TENANT ROW</th></tr></thead>
				<tbody
					>{#each conflicts as c (c.id)}<tr>
							<td>{c.kind}</td><td>{c.object}</td><td><code>{JSON.stringify(c.key)}</code></td><td
								>{c.tenant_row}</td
							>
						</tr>{:else}<tr><td colspan="4" class="empty">No conflicts.</td></tr>{/each}</tbody
				>
			</table>
		</div>
	</section>{/if}

<style>
	.targets {
		list-style: none;
		padding: 0;
		display: flex;
		gap: 8px;
		flex-wrap: wrap;
	}
	.targets button.active {
		font-weight: 700;
	}
	.new-target {
		display: grid;
		gap: 6px;
		max-width: 420px;
		margin-top: 12px;
	}
	label {
		font-size: 12px;
		font-weight: 600;
	}
	.actions {
		display: flex;
		gap: 8px;
		justify-content: flex-end;
	}
	.drift {
		display: flex;
		gap: 12px;
		align-items: center;
		margin-bottom: 12px;
	}
	.badge {
		border: 1px solid var(--line);
		border-radius: 999px;
		padding: 2px 10px;
		font-size: 12px;
	}
</style>
