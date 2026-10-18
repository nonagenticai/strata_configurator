import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
vi.mock('../../src/lib/api', async (original) => ({
	...(await original<typeof import('../../src/lib/api')>()),
	all: vi.fn(),
	api: vi.fn(),
}));
import Compile from '../../src/routes/compile/+page.svelte';
import { all, api, ApiError, type Layer, type Row } from '../../src/lib/api';
import { session } from '../../src/lib/session.svelte';

const base: Layer = { id: 'base', scope: 'base' };
const job: Layer = { id: 'job', scope: 'job', job: 'customer_care_agent' };
const tenantLayer: Layer = { id: 'tl', scope: 'tenant', tenant: 'acme' };
const tombstone: Row = {
	id: 'ts-1',
	kind: 'knowledge_store',
	key: 'returns-policies',
	op: 'tombstone',
	rev: 2,
	fields: {},
	is_current: true,
	author: 'alice',
	created_at: '2026-10-18T10:00:00Z',
};
/** A compiled config with one platform knowledge store, one tenant-added one and one platform entity. */
const compiled = {
	hash: 'h'.repeat(64),
	entities: { Order: { label: 'Order' } },
	attributes: {},
	relations: {},
	job: null,
	job_content: {
		context_spaces: {},
		procedures: {},
		connectors: {},
		knowledge_stores: {
			'faq-policies': { key: 'faq-policies', label: 'FAQ policies' },
			'acme-notes': { key: 'acme-notes', label: 'Acme notes' },
		},
	},
	warnings: [],
	provenance: {
		'entity:Order': [{ row_id: 'r-order', layer: { scope: 'base' }, op: 'add' }],
		'knowledge_store:faq-policies': [
			{ row_id: 'r-faq', layer: { scope: 'job', job: 'customer_care_agent' }, op: 'add' },
		],
		'knowledge_store:acme-notes': [
			{ row_id: 'r-acme', layer: { scope: 'tenant', tenant: 'acme' }, op: 'add' },
		],
	},
};

let layers: Layer[];
let tenantRows: Row[];
const calls: { path: string; body: unknown; method?: string }[] = [];

afterEach(() => cleanup());
beforeEach(() => {
	calls.length = 0;
	layers = [base, job];
	tenantRows = [];
	session.superAdmin = false;
	session.activeTenant = '';
	session.workspace = { id: 'acme', name: 'Acme', role: 'tenant_admin' };
	vi.mocked(all).mockReset();
	vi.mocked(api).mockReset();
	vi.mocked(all).mockImplementation(async (path: string) => {
		if (path === '/api/layers' || path === '/api/layers?scope=tenant')
			return path.endsWith('tenant') ? layers.filter((l) => l.scope === 'tenant') : layers;
		if (path === '/api/layers/base/rows?kind=job') return [{ key: 'customer_care_agent', op: 'add' }];
		if (path === '/api/layers/tl/rows') return tenantRows;
		return [];
	});
	vi.mocked(api).mockImplementation(async (path: string, body?: unknown, options?: { method?: string }) => {
		calls.push({ path, body, method: options?.method });
		if (path === '/api/compile') return structuredClone(compiled);
		if (path === '/api/layers') {
			layers = [...layers, tenantLayer];
			return tenantLayer;
		}
		if (path === '/api/layers/tl/rows') {
			tenantRows = [{ ...tombstone, key: 'faq-policies', rev: 1, id: 'ts-new' }];
			return tenantRows[0];
		}
		if (path.startsWith('/api/layers/tl/rows/')) {
			tenantRows = [];
			return undefined;
		}
		throw new Error(`unexpected ${path}`);
	});
});

async function compileAndOpenKnowledgeStores() {
	const view = render(Compile);
	const button = view.getByRole('button', { name: /Compile configuration/ });
	await waitFor(() => expect(button.hasAttribute('disabled')).toBe(false));
	await fireEvent.submit(button.closest('form')!);
	await view.findByText('h'.repeat(64));
	await fireEvent.click(view.getByRole('button', { name: 'Knowledge stores' }));
	return view;
}

it('lists job content and hides a platform item: creates the tenant layer, then a tombstone row', async () => {
	const view = await compileAndOpenKnowledgeStores();
	expect(view.getByText('faq-policies')).toBeTruthy();
	await fireEvent.click(view.getByRole('button', { name: 'Hide faq-policies in this tenant' }));
	await waitFor(() => expect(calls.some((c) => c.path === '/api/layers/tl/rows')).toBe(true));
	expect(calls.find((c) => c.path === '/api/layers')?.body).toEqual({ scope: 'tenant' });
	expect(calls.find((c) => c.path === '/api/layers/tl/rows')?.body).toEqual({
		kind: 'knowledge_store',
		key: 'faq-policies',
		op: 'tombstone',
		fields: {},
	});
	// The tenant layer is created before the row that goes into it.
	expect(calls.findIndex((c) => c.path === '/api/layers')).toBeLessThan(
		calls.findIndex((c) => c.path === '/api/layers/tl/rows'),
	);
	expect(await view.findByRole('button', { name: 'Unhide faq-policies' })).toBeTruthy();
	expect(view.getByText(/takes effect in the next release build and push/i)).toBeTruthy();
});

it('reuses an existing tenant layer and never creates a second one', async () => {
	layers = [base, job, tenantLayer];
	const view = await compileAndOpenKnowledgeStores();
	await fireEvent.click(view.getByRole('button', { name: 'Hide faq-policies in this tenant' }));
	await waitFor(() => expect(calls.some((c) => c.path === '/api/layers/tl/rows')).toBe(true));
	expect(calls.some((c) => c.path === '/api/layers')).toBe(false);
});

it('recovers when another admin created the tenant layer first (409 layer_exists)', async () => {
	const inner = vi.mocked(api).getMockImplementation()!;
	vi.mocked(api).mockImplementation(async (path, body, options) => {
		if (path === '/api/layers') {
			calls.push({ path, body });
			layers = [...layers, tenantLayer];
			throw new ApiError('layer_exists', 'a layer with this key already exists');
		}
		return inner(path, body, options);
	});
	const view = await compileAndOpenKnowledgeStores();
	await fireEvent.click(view.getByRole('button', { name: 'Hide faq-policies in this tenant' }));
	await waitFor(() => expect(calls.some((c) => c.path === '/api/layers/tl/rows')).toBe(true));
	expect(view.queryByRole('alert')).toBeNull();
});

it('shows hidden items and unhides one by retiring its tombstone row', async () => {
	layers = [base, job, tenantLayer];
	tenantRows = [tombstone];
	const view = await compileAndOpenKnowledgeStores();
	expect(view.getByText('returns-policies')).toBeTruthy();
	expect(view.getByText('Hidden in this tenant')).toBeTruthy();
	await fireEvent.click(view.getByRole('button', { name: 'Unhide returns-policies' }));
	await waitFor(() =>
		expect(calls).toContainEqual({
			path: '/api/layers/tl/rows/knowledge_store/returns-policies?expected_rev=2',
			body: undefined,
			method: 'DELETE',
		}),
	);
	await waitFor(() => expect(view.queryByRole('button', { name: 'Unhide returns-policies' })).toBeNull());
});

it('offers no Hide for an item the tenant added itself', async () => {
	const view = await compileAndOpenKnowledgeStores();
	expect(view.getByText('acme-notes')).toBeTruthy();
	expect(view.queryByRole('button', { name: 'Hide acme-notes in this tenant' })).toBeNull();
});

it('a tenant_user sees hidden state but no Hide or Unhide buttons', async () => {
	session.workspace = { id: 'acme', name: 'Acme', role: 'tenant_user' };
	layers = [base, job, tenantLayer];
	tenantRows = [tombstone];
	const view = await compileAndOpenKnowledgeStores();
	expect(view.getByText('returns-policies')).toBeTruthy();
	expect(view.getByText('Hidden in this tenant')).toBeTruthy();
	expect(view.queryByRole('button', { name: /^Hide / })).toBeNull();
	expect(view.queryByRole('button', { name: /^Unhide / })).toBeNull();
});

it('a super-admin acting in a tenant may hide; in the platform view nobody may', async () => {
	session.superAdmin = true;
	session.workspace = null;
	session.activeTenant = 'acme';
	let view = await compileAndOpenKnowledgeStores();
	expect(view.getByRole('button', { name: 'Hide faq-policies in this tenant' })).toBeTruthy();
	cleanup();
	session.activeTenant = '';
	view = await compileAndOpenKnowledgeStores();
	expect(view.getByText('faq-policies')).toBeTruthy();
	expect(view.queryByRole('button', { name: /^Hide / })).toBeNull();
});

it('hides ontology items too (entity tab)', async () => {
	const view = render(Compile);
	const button = view.getByRole('button', { name: /Compile configuration/ });
	await waitFor(() => expect(button.hasAttribute('disabled')).toBe(false));
	await fireEvent.submit(button.closest('form')!);
	await fireEvent.click(await view.findByRole('button', { name: 'Hide Order in this tenant' }));
	await waitFor(() =>
		expect(calls.find((c) => c.path === '/api/layers/tl/rows')?.body).toEqual({
			kind: 'entity',
			key: 'Order',
			op: 'tombstone',
			fields: {},
		}),
	);
});
