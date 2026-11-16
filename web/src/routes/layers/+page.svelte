<script lang="ts">
	import { page } from '$app/state';
	import { all, layerLabel, type Layer, type Row } from '#lib/api';
	import { session } from '#lib/session.svelte';
	let layers = $state<Layer[]>([]),
		selected = $state<Layer | null>(null),
		rows = $state<Row[]>([]),
		history = $state<Row[]>([]),
		historyKey = $state('');
	let loading = $state(true),
		rowsLoading = $state(false),
		historyLoading = $state(false),
		error = $state(''),
		search = $state('');
	let generation = 0,
		rowGeneration = 0,
		historyGeneration = 0;
	let drawer = $state<HTMLDialogElement>();
	$effect(() => {
		if (drawer && historyKey && !drawer.open) drawer.showModal();
	});
	const scopes = ['base', 'profile', 'industry', 'industry_dept', 'job', 'tenant'];
	let visibleRows = $derived(rows.filter((r) => r.key.toLowerCase().includes(search.toLowerCase())));
	$effect(() => {
		const activeTenant = session.activeTenant,
			target = page.url.searchParams.get('row');
		const current = ++generation;
		++rowGeneration;
		++historyGeneration;
		layers = [];
		selected = null;
		rows = [];
		history = [];
		historyKey = '';
		error = '';
		loading = true;
		const controller = new AbortController();
		async function load() {
			try {
				const found = await all<Layer>('/api/layers', { activeTenant, signal: controller.signal });
				if (current !== generation) return;
				layers = found.sort(
					(a, b) =>
						scopes.indexOf(a.scope) - scopes.indexOf(b.scope) || layerLabel(a).localeCompare(layerLabel(b)),
				);
				if (target) {
					for (const layer of layers) {
						const values = await all<Row>(`/api/layers/${layer.id}/rows`, {
							activeTenant,
							signal: controller.signal,
						});
						if (current !== generation) return;
						if (values.some((r) => r.id === target)) {
							selected = layer;
							rows = values;
							return;
						}
					}
					error =
						'This row is no longer current or is not visible in this workspace. Select a layer to inspect its revision history.';
				} else if (layers[0]) await choose(layers[0]);
			} catch (e) {
				if (current === generation && !controller.signal.aborted)
					error = e instanceof Error ? e.message : String(e);
			} finally {
				if (current === generation) loading = false;
			}
		}
		void load();
		return () => controller.abort();
	});
	async function choose(layer: Layer) {
		const current = ++rowGeneration;
		++historyGeneration;
		selected = layer;
		rows = [];
		history = [];
		historyKey = '';
		rowsLoading = true;
		search = '';
		error = '';
		try {
			const value = await all<Row>(`/api/layers/${layer.id}/rows`);
			if (current === rowGeneration) rows = value;
		} catch (e) {
			if (current === rowGeneration) error = e instanceof Error ? e.message : String(e);
		} finally {
			if (current === rowGeneration) rowsLoading = false;
		}
	}
	async function revisions(row: Row) {
		const current = ++historyGeneration;
		historyKey = row.key;
		history = [];
		historyLoading = true;
		try {
			const value = await all<Row>(
				`/api/layers/${selected?.id}/rows/${row.kind}/${encodeURIComponent(row.key)}/revisions`,
			);
			if (current === historyGeneration) history = value;
		} catch (e) {
			if (current === historyGeneration) error = e instanceof Error ? e.message : String(e);
		} finally {
			if (current === historyGeneration) historyLoading = false;
		}
	}
</script>

<div class="page-heading">
	<div>
		<div class="eyebrow">STANDARD MODEL</div>
		<h1>Explore the layers</h1>
		<p class="lede">Follow the model from shared foundations to tenant-specific changes.</p>
	</div>
	<span class="pill">{layers.length} layers</span>
</div>
{#if error}<div class="error" role="alert">{error}</div>{/if}
<div class="layers-grid">
	<section class="card layer-tree">
		<div class="card-header"><h2>Layer hierarchy</h2></div>
		<div class="tree-body">
			{#if loading}<p class="muted">Loading layers…</p>{/if}
			{#each scopes as scope}{@const group = layers.filter((l) => l.scope === scope)}{#if group.length}<div
						class="scope"
					>
						<span>{scope.replaceAll('_', ' ')}</span><small>{group.length}</small>
					</div>
					{#each group as layer}<button class:chosen={selected?.id === layer.id} onclick={() => choose(layer)}
							><span>▱</span>{layerLabel(layer).replace(`${scope}/`, '')}</button
						>{/each}{/if}{/each}
			{#if !loading && !layers.length}<p class="muted">No layers in this workspace.</p>{/if}
		</div>
	</section>
	<section class="card">
		<div class="card-header">
			<div>
				<h2>{selected ? layerLabel(selected) : 'Select a layer'}</h2>
				<p class="muted">Current overlay rows · changes are kept as revisions</p>
			</div>
			<span class="pill">{rows.length} rows</span>
		</div>
		<div class="toolbar">
			<label for="row-search" class="sr-only">Filter rows</label><input
				id="row-search"
				placeholder="Find an object…"
				bind:value={search}
			/>
		</div>
		{#if rowsLoading}<div class="empty">Loading rows…</div>{:else}<div class="table-wrap">
				<table>
					<thead><tr><th>OBJECT</th><th>OPERATION</th><th>REVISION</th><th>FIELDS</th><th></th></tr></thead
					><tbody
						>{#each visibleRows as row}<tr class:highlighted={row.id === page.url.searchParams.get('row')}
								><td
									><code>{row.key}</code>
									<div class="muted kind">{row.kind}</div></td
								><td><span class="pill op" class:deleted={row.op === 'tombstone'}>{row.op}</span></td><td
									>r{row.rev}</td
								><td
									><details>
										<summary>View fields</summary>
										<pre>{JSON.stringify(row.fields, null, 2)}</pre>
									</details></td
								><td><button class="history-button" onclick={() => revisions(row)}>History ↗</button></td
								></tr
							>{:else}<tr
								><td colspan="5" class="empty"
									>{selected
										? 'No matching rows in this layer.'
										: 'Choose a layer to explore its objects.'}</td
								></tr
							>{/each}</tbody
					>
				</table>
			</div>{/if}
	</section>
</div>
{#if historyKey}<dialog
		bind:this={drawer}
		class="drawer"
		aria-label={`Revision history for ${historyKey}`}
		oncancel={() => {
			++historyGeneration;
			historyKey = '';
		}}
	>
		<div class="card-header">
			<div>
				<div class="eyebrow">REVISION HISTORY</div>
				<h2>{historyKey}</h2>
			</div>
			<button
				class="close"
				onclick={() => {
					++historyGeneration;
					historyKey = '';
				}}>Close ×</button
			>
		</div>
		<div class="card-body">
			{#if historyLoading}<p class="muted">Loading revisions…</p>{/if}{#each history as row}<article>
					<div class="revision-title">
						<strong>Revision {row.rev}</strong><span class="pill"
							>{row.is_current ? 'Current' : 'Retired'} · {row.op}</span
						>
					</div>
					<p class="muted">{new Date(row.created_at).toLocaleString()} · {row.author}</p>
					<pre>{JSON.stringify(row.fields, null, 2)}</pre>
				</article>{/each}
		</div>
	</dialog>{/if}

<style>
	.layers-grid {
		display: grid;
		grid-template-columns: 250px minmax(0, 1fr);
		gap: 24px;
		align-items: start;
	}
	.tree-body {
		padding: 12px;
	}
	.scope {
		display: flex;
		justify-content: space-between;
		color: var(--muted);
		font-size: 10px;
		letter-spacing: 1px;
		text-transform: uppercase;
		padding: 20px 9px 9px;
	}
	.scope small {
		font-size: 9px;
	}
	.tree-body button {
		background: none;
		border: 0;
		width: 100%;
		text-align: left;
		color: #5c7266;
		font-size: 11px;
		font-weight: 400;
		padding: 10px;
		display: flex;
		gap: 10px;
		overflow-wrap: anywhere;
	}
	.tree-body button.chosen {
		background: var(--teal-light);
		color: var(--teal);
		font-weight: 600;
	}
	.card-header p {
		margin: 7px 0 0;
		font-size: 10px;
	}
	.toolbar {
		padding: 14px 22px;
		border-bottom: 1px solid var(--line);
	}
	.toolbar input {
		font-size: 11px;
		width: 250px;
	}
	.kind {
		font-size: 9px;
		margin-top: 7px;
	}
	.op {
		background: var(--teal-light);
		color: var(--teal);
	}
	.op.deleted {
		background: #fcf0e9;
		color: #995d42;
	}
	.history-button {
		background: none;
		color: var(--teal);
		border: 0;
		padding: 0;
		font-size: 10px;
		white-space: nowrap;
	}
	summary {
		cursor: pointer;
		font-size: 10px;
		color: var(--muted);
	}
	td pre {
		max-width: 250px;
	}
	.highlighted {
		background: #f1f8ee;
	}
	.drawer {
		position: fixed;
		inset: 0 0 0 auto;
		margin: 0;
		width: 530px;
		max-width: 100%;
		height: 100%;
		max-height: 100%;
		border: 0;
		padding: 0;
		overflow: auto;
		background: white;
		box-shadow: -10px 0 60px #12342520;
	}
	.drawer::backdrop {
		background: #16392a38;
	}
	.close {
		color: var(--muted);
		background: none;
		border: 1px solid var(--line);
		font-size: 11px;
	}
	.revision-title {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 10px;
		font-size: 13px;
	}
	article {
		padding: 20px 0;
		border-bottom: 1px solid var(--line);
	}
	@media (max-width: 1000px) {
		.layers-grid {
			grid-template-columns: 190px minmax(0, 1fr);
		}
	}
	@media (max-width: 700px) {
		.layers-grid {
			grid-template-columns: 1fr;
		}
		.layer-tree {
			max-height: 300px;
			overflow: auto;
		}
	}
</style>
