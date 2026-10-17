import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
vi.mock('../../src/lib/api', async (original) => ({
	...(await original<typeof import('../../src/lib/api')>()),
	all: vi.fn(),
	api: vi.fn(),
}));
import Tenants from '../../src/routes/tenants/+page.svelte';
import { all, api, ApiError } from '../../src/lib/api';
import { session } from '../../src/lib/session.svelte';

const acme = {
	id: '6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b',
	name: 'Acme',
	created_at: '2026-10-17T10:00:00Z',
	archived_at: null,
};

afterEach(() => cleanup());
beforeEach(() => {
	vi.mocked(all).mockReset();
	vi.mocked(api).mockReset();
	session.superAdmin = true;
	session.tenants = [{ id: acme.id, name: acme.name }];
	session.activeTenant = acme.id;
	// jsdom has no modal dialogs.
	HTMLDialogElement.prototype.showModal = function (this: HTMLDialogElement) {
		this.open = true;
	};
});

async function openDialog() {
	const view = render(Tenants);
	await view.findByText('Acme');
	expect(all).toHaveBeenCalledWith('/api/tenants', { activeTenant: '' });
	await fireEvent.click(view.getByRole('button', { name: 'Offboard Acme' }));
	const submit = view.getByRole('button', { name: 'Offboard this tenant' }) as HTMLButtonElement;
	const input = view.getByLabelText(/to confirm/) as HTMLInputElement;
	return { view, submit, input };
}

it('only offboards once the exact tenant name is typed', async () => {
	vi.mocked(all).mockResolvedValueOnce([acme]).mockResolvedValueOnce([]);
	vi.mocked(api).mockResolvedValue({ tenant: { ...acme, archived_at: '2026-10-18T00:00:00Z' } });
	const { view, submit, input } = await openDialog();
	expect(submit.disabled).toBe(true);
	for (const wrong of ['', 'acme', 'Acm', 'Acme Inc']) {
		await fireEvent.input(input, { target: { value: wrong } });
		expect(submit.disabled, wrong).toBe(true);
		await fireEvent.submit(submit.closest('form')!);
	}
	expect(api).not.toHaveBeenCalled();

	await fireEvent.input(input, { target: { value: ' Acme ' } });
	expect(submit.disabled).toBe(false);
	await fireEvent.submit(submit.closest('form')!);
	await waitFor(() =>
		expect(api).toHaveBeenCalledWith(
			`/api/tenants/${acme.id}/offboard`,
			{ confirm_name: 'Acme' },
			{ activeTenant: '' },
		),
	);
	expect(await view.findByText(/Offboarded “Acme”/)).toBeTruthy();
	expect(await view.findByText('No live tenants.')).toBeTruthy();
	// the switcher drops it and leaves it
	expect(session.tenants).toEqual([]);
	expect(session.activeTenant).toBe('');
});

it('shows the licence failure and keeps the tenant', async () => {
	vi.mocked(all).mockResolvedValue([acme]);
	vi.mocked(api).mockRejectedValue(
		new ApiError(
			'license_provisioning_failed',
			'revoking the tenant licence failed, so the tenant still holds it; retry',
		),
	);
	const { view, submit, input } = await openDialog();
	await fireEvent.input(input, { target: { value: 'Acme' } });
	await fireEvent.submit(submit.closest('form')!);
	const alert = await view.findByRole('alert');
	expect(alert.textContent).toContain('still holds it; retry');
	expect(view.getByRole('button', { name: 'Offboard this tenant' })).toBeTruthy();
	expect(session.tenants).toHaveLength(1);
	expect(session.activeTenant).toBe(acme.id);
});
