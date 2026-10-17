import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
vi.mock('../../src/lib/api', async (original) => ({
	...(await original<typeof import('../../src/lib/api')>()),
	all: vi.fn(),
	api: vi.fn(),
}));
import Compile from '../../src/routes/compile/+page.svelte';
import { all, api, ApiError } from '../../src/lib/api';
import { session } from '../../src/lib/session.svelte';
afterEach(cleanup);
beforeEach(() => {
	session.tenant = '';
	vi.mocked(all).mockImplementation(async (path) => {
		if (path === '/api/layers')
			return [
				{ id: 'base', scope: 'base' },
				{ id: 'retail', scope: 'industry', industry: 'retail' },
			];
		if (path === '/api/layers/base/rows?kind=job') return [{ key: 'customer_care_agent', op: 'add' }];
		if (path === '/api/dimensions') return [{ key: 'department', label: 'Department' }];
		if (path === '/api/dimensions/department/values')
			return ['faq', 'logistics', 'order-support', 'payments'].map((key) => ({ key, label: key }));
		return [];
	});
});
it('compiles the reference selection and shows its hash and provenance', async () => {
	vi.mocked(api).mockResolvedValue({
		hash: 'a'.repeat(64),
		entities: { Order: { label: 'Order' } },
		attributes: {},
		relations: {},
		job: null,
		warnings: [],
		provenance: {
			'entity:Order': [
				{ row_id: 'order-row', layer: { scope: 'base' }, op: 'add' },
				{ row_id: 'retail-row', layer: { scope: 'industry', industry: 'retail' }, op: 'patch' },
			],
		},
	});
	const view = render(Compile);
	const reference = view.getByRole('button', { name: 'Use retail reference selection' });
	await waitFor(() => expect(reference.hasAttribute('disabled')).toBe(false));
	await fireEvent.click(reference);
	await fireEvent.submit(view.getByRole('button', { name: /Compile configuration/ }).closest('form')!);
	expect(await view.findByText('a'.repeat(64))).toBeTruthy();
	expect(view.getByRole('link', { name: 'industry/retail patch' }).getAttribute('href')).toBe(
		'/layers?row=retail-row',
	);
	expect(api).toHaveBeenCalledWith(
		'/api/compile',
		expect.objectContaining({
			industry: 'retail',
			job: 'customer_care_agent',
			departments: ['faq', 'logistics', 'order-support', 'payments'],
		}),
	);
});
it('shows compile errors with a link to the offending row', async () => {
	vi.mocked(api).mockRejectedValue(
		new ApiError('compile_failed', 'Invalid configuration', [
			{ code: 'patch_missing', message: 'Order does not exist', row_id: 'bad-row' },
		]),
	);
	const view = render(Compile);
	await waitFor(() =>
		expect(view.getByRole('button', { name: /Compile configuration/ }).hasAttribute('disabled')).toBe(false),
	);
	await fireEvent.submit(view.getByRole('button', { name: /Compile configuration/ }).closest('form')!);
	expect(await view.findByRole('alert')).toBeTruthy();
	expect(view.getByRole('link', { name: /View row/ }).getAttribute('href')).toBe('/layers?row=bad-row');
});
