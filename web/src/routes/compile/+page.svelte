<script lang="ts">
	import {
		api,
		all,
		ApiError,
		type Layer,
		type Row,
		type EffectiveConfig,
		type CompileError,
		type Origin,
	} from '$lib/api';
	import { session } from '$lib/session.svelte';
	import Provenance from '$lib/Provenance.svelte';
	type Profile = { id: string; name: string; tenant: string | null; values: Record<string, string[]> };
	type Dimension = { key: string; label: string; values: { key: string; label: string }[] };
	let industries = $state<string[]>([]),
		jobs = $state<string[]>([]),
		profiles = $state<Profile[]>([]),
		dimensions = $state<Dimension[]>([]);
	let industry = $state(''),
		job = $state(''),
		profile = $state(''),
		departments = $state<string[]>([]),
		selectedDimensions = $state<Record<string, string>>({}),
		customProfile = $state('{}');
	let result = $state<EffectiveConfig | null>(null),
		error = $state(''),
		errors = $state<CompileError[]>([]),
		loading = $state(true),
		compiling = $state(false);
	/** One tab per overlay kind the compiled view lists; the value is the overlay `kind`. */
	const tabs = [
		['entity', 'Entities'],
		['attribute', 'Attributes'],
		['relation', 'Relations'],
		['context_space', 'Context spaces'],
		['procedure', 'Procedures'],
		['connector_slot', 'Connector slots'],
		['knowledge_store', 'Knowledge stores'],
	] as const;
	type Tab = (typeof tabs)[number][0];
	let tab = $state<Tab>('entity'),
		search = $state('');
	/** The active tenant's layer (null until it exists) and its current tombstones: what this tenant hides. */
	let tenantLayer = $state<Layer | null>(null),
		hiddenRows = $state<Row[]>([]),
		hiding = $state(''),
		hideError = $state(''),
		hideNotice = $state('');
	/** Acting in a tenant: a member always is; a super-admin when a tenant is selected or they have a home one. */
	let inTenant = $derived(
		session.superAdmin ? !!session.activeTenant || !!session.workspace : !!session.workspace,
	);
	/** Hiding needs tenant_admin in that tenant (a super-admin acting in it counts); tenant_user only sees state. */
	let canHide = $derived(inTenant && (session.superAdmin || session.workspace?.role === 'tenant_admin'));
	function itemsOf(r: EffectiveConfig, kind: Tab): Record<string, Record<string, unknown>> {
		const c = r.job_content;
		switch (kind) {
			case 'entity':
				return r.entities;
			case 'attribute':
				return r.attributes;
			case 'relation':
				return r.relations;
			case 'context_space':
				return c?.context_spaces ?? {};
			case 'procedure':
				return c?.procedures ?? {};
			case 'connector_slot':
				return c?.connectors ?? {};
			case 'knowledge_store':
				return c?.knowledge_stores ?? {};
		}
	}
	/** A base item: every row that built it is a platform (inherited) row, none is this tenant's own. */
	function isBase(origins: Origin[] | undefined): boolean {
		return !!origins?.length && origins.every((o) => o.layer.scope !== 'tenant');
	}
	let hiddenKeys = $derived(new Set(hiddenRows.map((r) => `${r.kind}:${r.key}`)));
	let hiddenEntries = $derived(
		hiddenRows.filter((r) => r.kind === tab && r.key.toLowerCase().includes(search.toLowerCase())),
	);
	let selectedProfile = $derived(profiles.find((p) => p.id === profile));
	let departmentOptions = $derived(dimensions.find((d) => d.key === 'department')?.values || []);
	let entries = $derived(
		result
			? Object.entries(itemsOf(result, tab)).filter(
					([key]) => key.toLowerCase().includes(search.toLowerCase()) && !hiddenKeys.has(`${tab}:${key}`),
				)
			: [],
	);
	let context = 0;
	$effect(() => {
		const activeTenant = session.activeTenant;
		const controller = new AbortController();
		const generation = ++context;
		loading = true;
		error = '';
		errors = [];
		result = null;
		industry = '';
		job = '';
		profile = '';
		departments = [];
		selectedDimensions = {};
		customProfile = '{}';
		compiling = false;
		tenantLayer = null;
		hiddenRows = [];
		hiding = '';
		hideError = '';
		hideNotice = '';
		async function load() {
			try {
				const opts = { activeTenant, signal: controller.signal };
				const [layers, templates, dims] = await Promise.all([
					all<Layer>('/api/layers', opts),
					all<Profile>('/api/profiles', opts),
					all<{ key: string; label: string }>('/api/dimensions', opts),
				]);
				const [baseRows, resolvedDims] = await Promise.all([
					Promise.all(
						layers
							.filter((l) => l.scope === 'base')
							.map((l) => all<Row>(`/api/layers/${l.id}/rows?kind=job`, opts)),
					),
					Promise.all(
						dims.map(async (d) => ({
							...d,
							values: await all<{ key: string; label: string }>(
								`/api/dimensions/${encodeURIComponent(d.key)}/values`,
								opts,
							),
						})),
					),
				]);
				const ownLayer = layers.find((l) => l.scope === 'tenant') ?? null;
				const ownRows = ownLayer ? await all<Row>(`/api/layers/${ownLayer.id}/rows`, opts) : [];
				if (controller.signal.aborted) return;
				tenantLayer = ownLayer;
				hiddenRows = ownRows.filter((r) => r.op === 'tombstone');
				industries = [...new Set(layers.map((l) => l.industry).filter((v): v is string => !!v))].sort();
				jobs = [
					...new Set(
						baseRows
							.flat()
							.filter((r) => r.op === 'add')
							.map((r) => r.key),
					),
				].sort();
				profiles = templates;
				dimensions = resolvedDims;
			} catch (e) {
				if (!controller.signal.aborted) error = e instanceof Error ? e.message : String(e);
			} finally {
				if (generation === context) loading = false;
			}
		}
		void load();
		return () => controller.abort();
	});
	/** `keep`: leave the current result on screen until the new one arrives (a recompile after hide/unhide). */
	async function compile(keep = false) {
		compiling = true;
		error = '';
		errors = [];
		if (!keep) result = null;
		const generation = context;
		try {
			const body = {
				industry: industry || undefined,
				job: job || undefined,
				departments,
				...(profile === 'custom'
					? { profile: JSON.parse(customProfile) }
					: profile
						? { profile_id: profile }
						: {}),
				dimensions: Object.fromEntries(Object.entries(selectedDimensions).filter(([, v]) => !!v)),
			};
			const value = await api<EffectiveConfig>('/api/compile', body);
			if (generation === context) result = value;
		} catch (e) {
			if (generation === context) {
				result = null;
				error = e instanceof Error ? e.message : String(e);
				errors = e instanceof ApiError ? e.errors : [];
			}
		} finally {
			if (generation === context) compiling = false;
		}
	}
	/** The tenant's layer, created on first use; a concurrent creator's 409 `layer_exists` reuses theirs. */
	async function ensureTenantLayer(): Promise<Layer> {
		if (tenantLayer) return tenantLayer;
		let layer: Layer | undefined;
		try {
			layer = await api<Layer>('/api/layers', { scope: 'tenant' });
		} catch (e) {
			if (!(e instanceof ApiError && e.code === 'layer_exists')) throw e;
			layer = (await all<Layer>('/api/layers?scope=tenant')).find((l) => l.scope === 'tenant');
			if (!layer) throw e;
		}
		tenantLayer = layer;
		return layer;
	}
	/** Run a hide/unhide write, then recompile so the preview shows the effect. */
	async function change(id: string, write: () => Promise<string>) {
		const generation = context;
		hiding = id;
		hideError = '';
		hideNotice = '';
		try {
			const notice = await write();
			if (generation !== context) return;
			hideNotice = notice;
			if (result) await compile(true);
		} catch (e) {
			if (generation === context) hideError = e instanceof Error ? e.message : String(e);
		} finally {
			if (generation === context) hiding = '';
		}
	}
	function hide(kind: Tab, key: string) {
		return change(`${kind}:${key}`, async () => {
			const layer = await ensureTenantLayer();
			const row = await api<Row>(`/api/layers/${layer.id}/rows`, { kind, key, op: 'tombstone', fields: {} });
			hiddenRows = [...hiddenRows, row];
			return `${key} is now hidden in this tenant.`;
		});
	}
	function unhide(row: Row) {
		return change(`${row.kind}:${row.key}`, async () => {
			await api<void>(
				`/api/layers/${tenantLayer?.id}/rows/${row.kind}/${encodeURIComponent(row.key)}?expected_rev=${row.rev}`,
				undefined,
				{ method: 'DELETE' },
			);
			hiddenRows = hiddenRows.filter((r) => r.id !== row.id);
			return `${row.key} is visible again in this tenant.`;
		});
	}
	function useGolden() {
		industry = 'retail';
		departments = ['faq', 'logistics', 'order-support', 'payments'];
		job = 'customer_care_agent';
		profile = '';
		selectedDimensions = {};
	}
</script>

<div class="page-heading">
	<div>
		<div class="eyebrow">MODEL EXPLORER</div>
		<h1>Compile a configuration</h1>
		<p class="lede">Choose the parts. See the effective model and where every value comes from.</p>
	</div>
	<span class="pill">Read-only preview</span>
</div>
{#if error}<div class="error" role="alert">
		<strong>Configuration needs attention</strong>
		<p>{error}</p>
		{#if errors.length}<ul>
				{#each errors as e}<li>
						<code>{e.code}</code> · {e.message}
						{#if e.row_id}<a href={`/layers?row=${encodeURIComponent(e.row_id)}`}>View row ↗</a>{/if}
					</li>{/each}
			</ul>{/if}
	</div>{/if}
<div class="compile-grid">
	<section class="card selection">
		<div class="card-header">
			<h2>Your selection</h2>
			<span class="step">01</span>
		</div>
		<form
			onsubmit={(e) => {
				e.preventDefault();
				void compile();
			}}
			class="card-body"
		>
			{#if loading}<p class="muted">Loading available parts…</p>{/if}
			<div class="field">
				<label for="industry">Industry</label><select id="industry" bind:value={industry} disabled={loading}
					><option value="">Base model only</option>{#each industries as v}<option value={v}>{v}</option
						>{/each}</select
				>
			</div>
			<div class="field">
				<label for="job">Job</label><select
					id="job"
					bind:value={job}
					disabled={loading}
					onchange={() => {
						if (!job) selectedDimensions = {};
					}}
					><option value="">All ontology · no job slice</option>{#each jobs as v}<option value={v}
							>{v.replaceAll('_', ' ')}</option
						>{/each}</select
				>
			</div>
			<div class="field">
				<fieldset>
					<legend>Departments</legend>
					<div class="checks">
						{#each departmentOptions as v}<label
								><input
									type="checkbox"
									bind:group={departments}
									value={v.key}
									disabled={!industry}
								/>{v.label}</label
							>{/each}
					</div>
				</fieldset>
			</div>
			<div class="field">
				<label for="profile">Classifier profile</label><select id="profile" bind:value={profile}
					><option value="">No classifier profile</option>{#each profiles as p}<option value={p.id}
							>{p.name}{p.tenant ? '' : ' · template'}</option
						>{/each}<option value="custom">Custom profile…</option></select
				>
			</div>
			{#if selectedProfile}<details class="profile-values">
					<summary>View profile values</summary>
					<pre>{JSON.stringify(selectedProfile.values, null, 2)}</pre>
				</details>{/if}
			{#if profile === 'custom'}<div class="field">
					<label for="custom-profile">Axis values (JSON)</label><textarea
						id="custom-profile"
						bind:value={customProfile}
						spellcheck="false"
					></textarea>
				</div>{/if}
			{#each dimensions.filter((d) => d.key !== 'department') as d}<div class="field">
					<label for={`dim-${d.key}`}>{d.label}</label><select
						id={`dim-${d.key}`}
						bind:value={selectedDimensions[d.key]}
						disabled={!job}
						><option value="">Any {d.label.toLowerCase()}</option>{#each d.values as v}<option value={v.key}
								>{v.label}</option
							>{/each}</select
					>
				</div>{/each}
			<button class="compile-button" disabled={loading || compiling} type="submit"
				>{compiling ? 'Compiling…' : 'Compile configuration'} <span>↗</span></button
			>
			<button class="golden" type="button" disabled={loading || compiling} onclick={useGolden}
				>Use retail reference selection</button
			>
		</form>
	</section>
	<div class="result-area">
		{#if result}
			<div class="stats">
				{#each [['Entities', Object.keys(result.entities).length], ['Attributes', Object.keys(result.attributes).length], ['Relations', Object.keys(result.relations).length]] as [label, n]}<div
						class="card stat"
					>
						<span>{label}</span><strong>{n}</strong>
					</div>{/each}
			</div>
			<section class="card result">
				<div class="card-header">
					<h2>Effective model</h2>
					<span class="pill"><span class="status-dot"></span>Compiled</span>
				</div>
				<div class="result-tools">
					<div class="tabs" role="group" aria-label="Object kind">
						{#each tabs as [value, label]}<button
								class:chosen={tab === value}
								onclick={() => {
									tab = value;
									search = '';
								}}>{label}</button
							>{/each}
					</div>
					<label class="sr-only" for="search">Find an object</label><input
						id="search"
						placeholder="Find an object…"
						bind:value={search}
					/>
				</div>
				{#if hideError}<div class="error hide-error" role="alert">{hideError}</div>{/if}
				{#if hideNotice}<p class="hide-notice" role="status">{hideNotice}</p>{/if}
				<div class="table-wrap">
					<table>
						<thead
							><tr
								><th>OBJECT</th><th>DETAILS</th><th>PROVENANCE</th>{#if inTenant}<th
										><span class="sr-only">Visibility</span></th
									>{/if}</tr
							></thead
						><tbody
							>{#each entries as [key, value]}{@const origins = result.provenance[`${tab}:${key}`]}<tr
									><td><code class="object-key">{key}</code></td><td
										><span
											>{String(
												value.label ||
													value.name ||
													value.data_type ||
													value.cardinality ||
													value.status ||
													'',
											)}</span
										>{#if value.pii}<span class="pill">PII</span>{/if}</td
									><td><Provenance {origins} /></td>{#if inTenant}<td class="visibility"
											>{#if canHide && isBase(origins)}<button
													class="hide-button"
													aria-label={`Hide ${key} in this tenant`}
													disabled={!!hiding}
													onclick={() => hide(tab, key)}
													>{hiding === `${tab}:${key}` ? 'Hiding…' : 'Hide in this tenant'}</button
												>{/if}</td
										>{/if}</tr
								>{/each}{#each hiddenEntries as row}<tr class="hidden-row"
									><td><code class="object-key">{row.key}</code></td><td colspan="2"
										><span class="pill hidden-pill">Hidden in this tenant</span></td
									><td class="visibility"
										>{#if canHide}<button
												class="hide-button"
												aria-label={`Unhide ${row.key}`}
												disabled={!!hiding}
												onclick={() => unhide(row)}
												>{hiding === `${row.kind}:${row.key}` ? 'Unhiding…' : 'Unhide'}</button
											>{/if}</td
									></tr
								>{/each}{#if !entries.length && !hiddenEntries.length}<tr
									><td colspan={inTenant ? 4 : 3} class="empty">No matching objects.</td></tr
								>{/if}</tbody
						>
					</table>
				</div>
			</section>
			{#if inTenant}<p class="muted hide-hint">
					Hiding an inherited item removes it from this tenant's preview now; it takes effect in the next
					release build and push.
				</p>{/if}
			<div class="hash">
				<span class="status-dot"></span><span>CONFIGURATION HASH</span><code>{result.hash}</code>
			</div>
			{#if result.warnings.length}<section class="card card-body">
					<h2>Warnings</h2>
					{#each result.warnings as warning}<p class="muted">
							<code>{warning.code}</code> · {warning.key}
						</p>{/each}
				</section>{/if}
			{#if result.job}<details class="card card-body job-details">
					<summary>Job definition · {result.job.label || result.job.key}</summary>
					<pre>{JSON.stringify(result.job, null, 2)}</pre>
				</details>{/if}
		{:else}<section class="card result-placeholder">
				<div class="placeholder-icon">◈</div>
				<h2>Your configuration starts here</h2>
				<p>
					Select an industry, job and profile, then compile.<br />Explore the model with a complete trail of
					its origins.
				</p>
				<div class="layer-flow">
					<span>Base</span><i>→</i><span>Industry</span><i>→</i><span>Job</span><i>→</i><span>Tenant</span>
				</div>
				<span class="muted">Previewing never changes your model.</span>
			</section>{/if}
	</div>
</div>

<style>
	.compile-grid {
		display: grid;
		grid-template-columns: 300px minmax(0, 1fr);
		gap: 24px;
		align-items: start;
	}
	.step {
		font-size: 10px;
		color: var(--muted);
	}
	.field {
		margin-bottom: 22px;
	}
	.field select {
		width: 100%;
	}
	.compile-button {
		width: 100%;
		display: flex;
		justify-content: space-between;
		margin-top: 30px;
	}
	.golden {
		background: none;
		border: 0;
		color: var(--muted);
		font-size: 10px;
		font-weight: 400;
		width: 100%;
		padding: 14px 0 0;
	}
	.profile-values {
		font-size: 11px;
		color: var(--muted);
		margin: -8px 0 20px;
	}
	.stats {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		gap: 14px;
		margin-bottom: 20px;
	}
	.stat {
		padding: 18px 22px;
	}
	.stat span {
		font-size: 11px;
		color: var(--muted);
		display: block;
	}
	.stat strong {
		font-size: 30px;
		font-weight: 500;
		display: block;
		margin-top: 8px;
	}
	.result-tools {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: 16px 20px;
		border-bottom: 1px solid var(--line);
		gap: 12px;
	}
	.tabs {
		display: flex;
		gap: 3px;
	}
	.tabs button {
		border: 0;
		background: none;
		color: var(--muted);
		padding: 7px 10px;
		font-size: 11px;
	}
	.tabs button.chosen {
		background: var(--teal-light);
		color: var(--teal);
	}
	.result-tools input {
		font-size: 11px;
		padding: 8px 10px;
		width: 150px;
	}
	.visibility {
		text-align: right;
		white-space: nowrap;
	}
	.hide-button {
		background: none;
		color: var(--teal);
		border: 1px solid var(--line);
		font-size: 10px;
		padding: 4px 8px;
	}
	.hidden-row {
		opacity: 0.75;
	}
	.hidden-pill {
		background: #fcf0e9;
		color: #995d42;
	}
	.hide-error,
	.hide-notice {
		margin: 12px 20px 0;
		font-size: 11px;
	}
	.hide-hint {
		font-size: 10px;
		margin: 12px 0 0;
	}
	.object-key {
		color: var(--teal);
		font-weight: 600;
	}
	.hash {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 8px;
		margin: 18px 0 25px;
		font-size: 8px;
		color: var(--muted);
		letter-spacing: 1px;
	}
	.hash code {
		font-size: 9px;
		letter-spacing: 0;
		overflow-wrap: anywhere;
		word-break: break-all;
	}
	.hash .status-dot {
		margin: 0;
	}
	.result-placeholder {
		min-height: 480px;
		display: flex;
		align-items: center;
		justify-content: center;
		flex-direction: column;
		text-align: center;
		padding: 35px;
	}
	.placeholder-icon {
		font-size: 44px;
		color: #8fb4a2;
		background: #f1f6ef;
		border-radius: 18px;
		padding: 5px 20px;
		margin-bottom: 26px;
	}
	.result-placeholder p {
		font-size: 12px;
		color: var(--muted);
		margin: 14px 0 30px;
	}
	.result-placeholder h2 {
		font-size: 20px;
	}
	.layer-flow {
		display: flex;
		gap: 10px;
		align-items: center;
		margin-bottom: 40px;
		font-size: 10px;
	}
	.layer-flow span {
		background: #f5f7f2;
		border: 1px solid var(--line);
		border-radius: 5px;
		padding: 8px 10px;
		color: #6a7e70;
	}
	.layer-flow i {
		color: #aab9ad;
		font-style: normal;
	}
	.result-placeholder > .muted {
		font-size: 10px;
	}
	.job-details {
		margin-top: 15px;
		font-size: 12px;
	}
	summary {
		cursor: pointer;
	}
	@media (max-width: 1150px) {
		.compile-grid {
			grid-template-columns: 260px minmax(0, 1fr);
		}
		.result-tools {
			flex-wrap: wrap;
		}
		.result-tools input {
			width: 100%;
		}
	}
	@media (max-width: 900px) {
		.compile-grid {
			grid-template-columns: 1fr;
		}
		.selection form {
			display: grid;
			grid-template-columns: 1fr 1fr;
			gap: 0 20px;
		}
		.compile-button,
		.golden,
		.profile-values {
			grid-column: 1/-1;
		}
		.result-placeholder {
			min-height: 330px;
		}
		.result-tools input {
			width: 150px;
		}
	}
</style>
