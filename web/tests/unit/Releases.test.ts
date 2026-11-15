import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
vi.mock('../../src/lib/api', async (original) => ({
	...(await original<typeof import('../../src/lib/api')>()),
	all: vi.fn(),
	api: vi.fn(),
}));
import Releases from '../../src/routes/releases/+page.svelte';
import { all, api, ApiError } from '../../src/lib/api';
import { session } from '../../src/lib/session.svelte';

const target = {
	id: 't-1',
	job: 'customer_care_agent',
	selection: { industry: 'retail', departments: ['faq'] },
	created_by: 'alice',
	created_at: '2026-10-18T10:00:00Z',
};
const release = {
	id: 'r-1',
	target: 't-1',
	release_id: 'cfg-2026-10-18.1',
	hash: `sha256:${'a'.repeat(64)}`,
	created_by: 'alice',
	created_at: '2026-10-18T10:05:00Z',
};
const conflict = {
	id: 17,
	kind: 'base_removed',
	object: 'ontology_entity',
	key: { name: 'Carrier' },
	tenant_row: 'override',
};

afterEach(() => cleanup());
beforeEach(() => {
	vi.mocked(all).mockReset();
	vi.mocked(api).mockReset();
	session.superAdmin = false;
	session.activeTenant = '';
	session.workspace = { id: 'w-1', name: 'Acme', role: 'tenant_admin' };
	vi.mocked(all).mockResolvedValue([release]);
});

/** `api` answering GET /api/targets with one target, then `rest` by path. */
function routes(rest: Record<string, () => unknown>) {
	vi.mocked(api).mockImplementation(async (path: string) => {
		if (path === '/api/targets') return { items: [target], next: null };
		const handler = rest[path];
		if (!handler) throw new Error(`unexpected ${path}`);
		return handler();
	});
}

it('lists targets and releases', async () => {
	routes({});
	const view = render(Releases);
	expect(await view.findByText('cfg-2026-10-18.1')).toBeTruthy();
	expect(view.getAllByText('customer_care_agent').length).toBeGreaterThan(0);
	expect(all).toHaveBeenCalledWith('/api/targets/t-1/releases');
});

it('build shows violations on 422', async () => {
	routes({
		'/api/targets/t-1/releases': () => {
			throw new ApiError('bundle_incomplete', '2 problem(s); see errors', [
				{ path: '/bundle/context_spaces', message: 'the job has no context space' },
				{ path: '/bundle/procedures', message: 'the job has no PUBLISHED procedure' },
			] as never);
		},
	});
	const view = render(Releases);
	await view.findByText('cfg-2026-10-18.1');
	await fireEvent.click(view.getByRole('button', { name: 'Build release' }));
	const alert = await view.findByRole('alert');
	expect(alert.textContent).toContain('bundle_incomplete');
	expect(alert.textContent).toContain('/bundle/context_spaces');
	expect(alert.textContent).toContain('/bundle/procedures');
});

it('push renders conflicts', async () => {
	routes({
		'/api/releases/r-1/push': () => ({
			release,
			steps: [
				{ id: 1, step: 'tenant', status: 200, ok: true, response: {}, error_code: null, pushed_at: '' },
				{ id: 2, step: 'job', status: 200, ok: true, response: {}, error_code: null, pushed_at: '' },
				{
					id: 3,
					step: 'release',
					status: 201,
					ok: true,
					response: { conflicts: [conflict] },
					error_code: null,
					pushed_at: '',
				},
			],
			carebus: { applied: true, conflicts: [conflict] },
		}),
	});
	const view = render(Releases);
	await view.findByText('cfg-2026-10-18.1');
	await fireEvent.click(view.getByRole('button', { name: 'Push cfg-2026-10-18.1' }));
	expect(await view.findByText('base_removed')).toBeTruthy();
	expect(view.getByText('ontology_entity')).toBeTruthy();
	expect(view.getByText('{"name":"Carrier"}')).toBeTruthy();
	expect(api).toHaveBeenCalledWith('/api/releases/r-1/push', {});
});

it('push button hidden for tenant_user', async () => {
	session.workspace = { id: 'w-1', name: 'Acme', role: 'tenant_user' };
	routes({});
	const view = render(Releases);
	await view.findByText('cfg-2026-10-18.1');
	expect(view.queryByRole('button', { name: 'Push cfg-2026-10-18.1' })).toBeNull();
	expect(view.queryByRole('button', { name: 'Build release' })).toBeNull();
	expect(view.queryByRole('button', { name: 'Create target' })).toBeNull();
	expect(view.getByRole('button', { name: 'Check drift' })).toBeTruthy();
});

it('drift badge shows status', async () => {
	routes({
		'/api/targets/t-1/drift': () => ({
			status: 'drift',
			carebus: { release_id: '2026-10-20.1', hash: 'sha256:b', applied_at: '', conflicts: [] },
			last_pushed: { release_id: release.release_id, hash: release.hash, pushed_at: '' },
			latest_built: { release_id: release.release_id, hash: release.hash },
			config_changed: true,
		}),
	});
	const view = render(Releases);
	await view.findByText('cfg-2026-10-18.1');
	await fireEvent.click(view.getByRole('button', { name: 'Check drift' }));
	const badge = await view.findByTestId('drift-status');
	expect(badge.textContent).toBe('drift');
	await waitFor(() => expect(view.getByText(/care-bus holds 2026-10-20.1/)).toBeTruthy());
	expect(view.getByText(/configuration changed/)).toBeTruthy();
});
