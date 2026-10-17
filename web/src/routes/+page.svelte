<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	onMount(() => {
		void goto('/compile', { replaceState: true });
	});
</script>

<p class="muted">Opening Compile…</p>
