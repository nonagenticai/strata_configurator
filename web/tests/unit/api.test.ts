import { beforeEach, describe, expect, it, vi } from 'vitest';
vi.mock('../../src/lib/auth', () => ({ getToken: vi.fn().mockResolvedValue('fresh-token') }));
import { api, all, ApiError } from '../../src/lib/api';
import { session } from '../../src/lib/session.svelte';

describe('API client', () => {
	beforeEach(() => {
		vi.unstubAllGlobals();
		session.tenant = 'tenant-a';
	});
	it('sends X-Tenant-Id and Bearer', async () => {
		const fetch = vi.fn().mockResolvedValue(new Response('{"ok":true}'));
		vi.stubGlobal('fetch', fetch);
		await api('/api/compile', {});
		expect(fetch.mock.calls[0]?.[1].headers).toEqual({
			Authorization: 'Bearer fresh-token',
			'X-Tenant-Id': 'tenant-a',
			'Content-Type': 'application/json',
		});
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
	it('refreshes once after a 401', async () => {
		const fetch = vi
			.fn()
			.mockResolvedValueOnce(new Response('{}', { status: 401 }))
			.mockResolvedValueOnce(new Response('{"items":[]}'));
		vi.stubGlobal('fetch', fetch);
		await api('/api/layers');
		expect(fetch).toHaveBeenCalledTimes(2);
	});
	it('keeps one tenant across paginated requests', async () => {
		const fetch = vi
			.fn()
			.mockImplementationOnce(() => {
				session.tenant = 'tenant-b';
				return Promise.resolve(new Response('{"items":[1],"next":"cursor"}'));
			})
			.mockResolvedValueOnce(new Response('{"items":[2],"next":null}'));
		vi.stubGlobal('fetch', fetch);
		expect(await all('/api/layers')).toEqual([1, 2]);
		expect(fetch.mock.calls[1]?.[1].headers['X-Tenant-Id']).toBe('tenant-a');
		expect(fetch.mock.calls[1]?.[0]).toContain('before=cursor');
	});
});
