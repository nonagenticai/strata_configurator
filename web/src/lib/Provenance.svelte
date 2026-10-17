<script lang="ts">
	import { layerLabel, type Origin } from './api';
	let { origins = [] }: { origins?: Origin[] } = $props();
</script>

<span class="origins" aria-label="Provenance">
	{#each origins as origin, i}
		{#if i}<span class="arrow" aria-hidden="true">→</span>{/if}
		<a class="origin" href={`/layers?row=${encodeURIComponent(origin.row_id)}`} title={origin.row_id}
			>{layerLabel(origin.layer)} <strong>{origin.op}</strong></a
		>
	{/each}
</span>

<style>
	.origins {
		display: inline-flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 5px;
		font-size: 11px;
	}
	.origin {
		background: var(--teal-light);
		color: var(--teal);
		border: 1px solid #ccdfd9;
		border-radius: 5px;
		padding: 3px 6px;
		text-decoration: none;
	}
	.origin:hover {
		border-color: var(--teal);
	}
	.origin strong {
		font-weight: 600;
	}
	.arrow {
		color: var(--muted);
	}
</style>
