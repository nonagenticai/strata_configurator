<script lang="ts">
	import {
		api,
		all,
		ApiError,
		type Layer,
		type Row,
		type EffectiveConfig,
		type CompileError,
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
	let tab = $state('entity'),
		search = $state('');
	let selectedProfile = $derived(profiles.find((p) => p.id === profile));
	let departmentOptions = $derived(dimensions.find((d) => d.key === 'department')?.values || []);
	let entries = $derived(
		result
			? Object.entries(
					tab === 'entity' ? result.entities : tab === 'attribute' ? result.attributes : result.relations,
				).filter(([key]) => key.toLowerCase().includes(search.toLowerCase()))
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
				if (controller.signal.aborted) return;
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
	async function compile() {
		compiling = true;
		error = '';
		errors = [];
		result = null;
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
				error = e instanceof Error ? e.message : String(e);
				errors = e instanceof ApiError ? e.errors : [];
			}
		} finally {
			if (generation === context) compiling = false;
		}
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
						{#each [['entity', 'Entities'], ['attribute', 'Attributes'], ['relation', 'Relations']] as [value, label]}<button
								class:chosen={tab === value}
								onclick={() => {
									tab = value!;
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
				<div class="table-wrap">
					<table>
						<thead><tr><th>OBJECT</th><th>DETAILS</th><th>PROVENANCE</th></tr></thead><tbody
							>{#each entries as [key, value]}<tr
									><td><code class="object-key">{key}</code></td><td
										><span>{String(value.label || value.data_type || value.cardinality || '')}</span
										>{#if value.pii}<span class="pill">PII</span>{/if}</td
									><td><Provenance origins={result.provenance[`${tab}:${key}`]} /></td></tr
								>{:else}<tr><td colspan="3" class="empty">No matching objects.</td></tr>{/each}</tbody
						>
					</table>
				</div>
			</section>
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
