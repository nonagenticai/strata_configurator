<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { initAuth, logout } from '$lib/auth';
	import { api, all } from '$lib/api';
	import { roleLabel, session, type Workspace } from '$lib/session.svelte';
	import './app.css';
	let { data, children } = $props();
	let error = $state('');
	onMount(() => {
		let active = true;
		async function start() {
			try {
				const name = await initAuth(data.config);
				const me = await api<{
					sub: string;
					super_admin: boolean;
					tenant: Workspace | null;
				}>('/api/me', undefined, { activeTenant: '' });
				const tenants = me.super_admin
					? await all<{ id: string; name: string }>('/api/tenants', { activeTenant: '' })
					: [];
				if (!active) return;
				session.name = name;
				session.superAdmin = me.super_admin;
				session.workspace = me.tenant;
				session.tenants = tenants;
				session.activeTenant = '';
				session.ready = true;
			} catch (e) {
				if (active) error = e instanceof Error ? e.message : String(e);
			}
		}
		void start();
		return () => {
			active = false;
			session.ready = false;
		};
	});
</script>

<svelte:head
	><title>Strata · Configurator</title><meta
		name="description"
		content="Explore your standard model, compile a configuration and trace every change."
	/></svelte:head
>
<div class="shell">
	<aside>
		<a class="brand" href="/compile"
			><span class="brand-mark">≋</span> strata<span class="brand-dot">.</span></a
		>
		<div class="app-name">CONFIGURATOR</div>
		<div class="nav-label">WORKSPACE</div>
		<nav aria-label="Main navigation">
			<a href="/compile" class:active={page.url.pathname === '/compile' || page.url.pathname === '/'}
				><span>◈</span> Compile</a
			>
			<a href="/layers" class:active={page.url.pathname === '/layers'}><span>▱</span> Layers</a>
		</nav>
		{#if session.superAdmin}<div class="nav-label">PLATFORM</div>
			<nav aria-label="Platform navigation">
				<a href="/tenants" class:active={page.url.pathname === '/tenants'}><span>▣</span> Tenants</a>
				<a href="/signups" class:active={page.url.pathname === '/signups'}><span>✉</span> Signups</a>
			</nav>{/if}
		<div class="sidebar-note">
			<span class="status-dot"></span> Standard model · v1
			<p>Assembled from standard parts.<br />Configured for your organisation.</p>
		</div>
	</aside>
	<div class="workspace">
		<header>
			<span class="breadcrumb"
				>Workspace <span>/</span>
				<strong
					>{page.url.pathname === '/layers'
						? 'Layers'
						: page.url.pathname === '/signups'
							? 'Signups'
							: page.url.pathname === '/tenants'
								? 'Tenants'
								: 'Compile'}</strong
				></span
			>
			<div class="account">
				{#if session.ready}
					{#if session.superAdmin}
						<label for="tenant" class="sr-only">Tenant</label>
						<select id="tenant" bind:value={session.activeTenant}>
							<option value=""
								>{session.workspace ? `${session.workspace.name} · Home` : 'Platform standards'}</option
							>
							{#each session.tenants as t}<option value={t.id}>{t.name} · Super-admin</option>{/each}
						</select>
					{:else if session.workspace}
						<span class="workspace-name" data-testid="workspace"
							>{session.workspace.name} · {roleLabel(session.workspace.role)}</span
						>
					{/if}
					<span class="user-name">{session.name}</span><button class="text-button" onclick={() => logout()}
						>Sign out</button
					>
				{:else}<span class="muted">Connecting to workspace…</span>{/if}
			</div>
		</header>
		<main>
			{#if error}<div class="error" role="alert">
					<h2>Unable to open workspace</h2>
					<p>{error}</p>
					<button onclick={() => location.reload()}>Try again</button>
				</div>
			{:else if !session.ready}<div class="welcome">
					<div class="eyebrow">STRATA CONFIGURATOR</div>
					<h1>Your standard model.<br />Your configuration.</h1>
					<p>Signing you in securely…</p>
				</div>
			{:else if !session.superAdmin && !session.workspace}<div class="empty" role="status">
					<h2>No workspace</h2>
					<p>Your account is not assigned to a workspace yet. Ask your administrator to add you to one.</p>
				</div>
			{:else}{@render children()}{/if}
		</main>
	</div>
</div>
