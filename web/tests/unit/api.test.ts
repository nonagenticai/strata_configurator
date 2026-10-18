import { beforeEach, describe, expect, it, vi } from 'vitest';
vi.mock('../../src/lib/auth', () => ({ getToken: vi.fn().mockResolvedValue('fresh-token') }));
import { api, all, ApiError } from '../../src/lib/api';
import { roleLabel, session } from '../../src/lib/session.svelte';

describe('API client', () => {
	beforeEach(() => {
		vi.unstubAllGlobals();
		session.superAdmin = true;
		session.activeTenant = 'tenant-a';
	});
	it('sends X-Active-Tenant-Id and Bearer for a super-admin with a selected tenant', async () => {
		const fetch = vi.fn().mockResolvedValue(new Response('{"ok":true}'));
		vi.stubGlobal('fetch', fetch);
		await api('/api/compile', {});
		expect(fetch.mock.calls[0]?.[1].headers).toEqual({
			Authorization: 'Bearer fresh-token',
			'X-Active-Tenant-Id': 'tenant-a',
			'Content-Type': 'application/json',
		});
	});
	it('sends no tenant header at Platform scope', async () => {
		session.activeTenant = '';
		const fetch = vi.fn().mockResolvedValue(new Response('{"ok":true}'));
		vi.stubGlobal('fetch', fetch);
		await api('/api/layers');
		expect(fetch.mock.calls[0]?.[1].headers).toEqual({ Authorization: 'Bearer fresh-token' });
	});
	it('never sends a tenant header for a normal user', async () => {
		session.superAdmin = false;
		const fetch = vi.fn().mockImplementation(() => Promise.resolve(new Response('{"items":[],"next":null}')));
		vi.stubGlobal('fetch', fetch);
		await api('/api/layers', undefined, { activeTenant: 'tenant-b' });
		await all('/api/layers');
		for (const call of fetch.mock.calls) {
			expect(call[1].headers).toEqual({ Authorization: 'Bearer fresh-token' });
		}
	});
	it('surfaces error.code and compile details', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				new Response(
					JSON.stringify({
						error: { code: 'compile_failed', message: 'Invalid overlay' },
						errors: [{ code: 'patch_missing', row_id: 'row-1' }],
					}),
					{ status: 422 },
				),
			),
		);
		const error = await api('/api/compile', {}).catch((e) => e);
		expect(error).toBeInstanceOf(ApiError);
		if (!(error instanceof ApiError)) throw new Error('expected API error');
		expect(error.code).toBe('compile_failed');
		expect(error.errors[0]?.row_id).toBe('row-1');
	});
	it('surfaces no_workspace', async () => {
		session.superAdmin = false;
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				new Response(JSON.stringify({ error: { code: 'no_workspace', message: 'No workspace' } }), {
					status: 403,
				}),
			),
		);
		const error = await api('/api/layers').catch((e) => e);
		expect(error).toBeInstanceOf(ApiError);
		expect((error as ApiError).code).toBe('no_workspace');
	});
	it('refreshes once after a 401 and keeps the selected tenant', async () => {
		const fetch = vi
			.fn()
			.mockResolvedValueOnce(new Response('{}', { status: 401 }))
			.mockResolvedValueOnce(new Response('{"items":[]}'));
		vi.stubGlobal('fetch', fetch);
		await api('/api/layers');
		expect(fetch).toHaveBeenCalledTimes(2);
		expect(fetch.mock.calls[1]?.[1].headers['X-Active-Tenant-Id']).toBe('tenant-a');
	});
	it('keeps one tenant across paginated requests', async () => {
		const fetch = vi
			.fn()
			.mockImplementationOnce(() => {
				session.activeTenant = 'tenant-b';
				return Promise.resolve(new Response('{"items":[1],"next":"cursor"}'));
			})
			.mockResolvedValueOnce(new Response('{"items":[2],"next":null}'));
		vi.stubGlobal('fetch', fetch);
		expect(await all('/api/layers')).toEqual([1, 2]);
		expect(fetch.mock.calls[1]?.[1].headers['X-Active-Tenant-Id']).toBe('tenant-a');
		expect(fetch.mock.calls[1]?.[0]).toContain('before=cursor');
	});
	it('sends DELETE without a body and accepts an empty 204', async () => {
		const fetch = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
		vi.stubGlobal('fetch', fetch);
		expect(
			await api('/api/layers/l/rows/entity/Order?expected_rev=1', undefined, { method: 'DELETE' }),
		).toBeUndefined();
		expect(fetch.mock.calls[0]?.[1].method).toBe('DELETE');
		expect(fetch.mock.calls[0]?.[1].body).toBeUndefined();
		expect(fetch.mock.calls[0]?.[1].headers['Content-Type']).toBeUndefined();
	});
	it('labels tenant roles', () => {
		expect(roleLabel('tenant_admin')).toBe('Tenant admin');
		expect(roleLabel('tenant_user')).toBe('Tenant user');
	});
});
