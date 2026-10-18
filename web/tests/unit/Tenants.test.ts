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

it('create tenant form posts name and first admin email', async () => {
	vi.mocked(all).mockResolvedValue([acme]);
	vi.mocked(api).mockResolvedValue({
		...acme,
		id: 'new-1',
		name: 'Globex',
		carebus: { ok: true, skipped: false, steps: [] },
	});
	const view = render(Tenants);
	await view.findByText('Acme');
	await fireEvent.input(view.getByLabelText('Tenant name', { selector: '#new-name' }), {
		target: { value: ' Globex ' },
	});
	await fireEvent.input(view.getByLabelText('First admin email'), { target: { value: 'ann@globex.com' } });
	await fireEvent.submit(view.getByRole('form', { name: 'Create tenant' }));
	await waitFor(() =>
		expect(api).toHaveBeenCalledWith(
			'/api/tenants',
			{ name: 'Globex', first_admin_email: 'ann@globex.com' },
			{ activeTenant: '' },
		),
	);
	expect(await view.findByText(/Created “Globex”. Provisioned into care-bus./)).toBeTruthy();
});

it('import posts fixed id and shows carebus outcome', async () => {
	vi.mocked(all).mockResolvedValue([acme]);
	const fr = { ...acme, id: '0195f48b-d000-7000-8000-000000000099', name: 'fr-test-user' };
	vi.mocked(api).mockResolvedValueOnce({
		tenant: fr,
		imported: true,
		carebus: {
			ok: false,
			skipped: false,
			steps: [{ step: 'tenant', status: null, ok: false, error_code: 'carebus_unavailable' }],
		},
	});
	const view = render(Tenants);
	await view.findByText('Acme');
	await fireEvent.input(view.getByLabelText('Tenant id'), { target: { value: fr.id } });
	await fireEvent.input(view.getByLabelText('Tenant name', { selector: '#import-name' }), {
		target: { value: 'fr-test-user' },
	});
	await fireEvent.input(view.getByLabelText('First admin (email or user id)'), {
		target: { value: 'fr-admin' },
	});
	await fireEvent.submit(view.getByRole('form', { name: 'Import existing tenant' }));
	await waitFor(() =>
		expect(api).toHaveBeenCalledWith(
			'/api/tenants/import',
			{ id: fr.id, name: 'fr-test-user', first_admin_sub: 'fr-admin' },
			{ activeTenant: '' },
		),
	);
	expect(await view.findByText(/Imported “fr-test-user”. care-bus provisioning failed/)).toBeTruthy();
	vi.mocked(api).mockResolvedValueOnce({ ok: true, skipped: false, steps: [] });
	await fireEvent.click(view.getByRole('button', { name: 'Retry care-bus provision' }));
	await waitFor(() =>
		expect(api).toHaveBeenCalledWith(`/api/tenants/${fr.id}/carebus-provision`, {}, { activeTenant: '' }),
	);
	expect(await view.findByText(/Provisioned into care-bus/)).toBeTruthy();
});

it('carebus outcome shows skipped when provisioning is off', async () => {
	vi.mocked(all).mockResolvedValue([acme]);
	vi.mocked(api).mockResolvedValue({
		...acme,
		id: 'new-2',
		name: 'Initech',
		carebus: { ok: false, skipped: true, steps: [] },
	});
	const view = render(Tenants);
	await view.findByText('Acme');
	await fireEvent.input(view.getByLabelText('Tenant name', { selector: '#new-name' }), {
		target: { value: 'Initech' },
	});
	await fireEvent.input(view.getByLabelText('First admin email'), { target: { value: 'bo@initech.com' } });
	await fireEvent.submit(view.getByRole('form', { name: 'Create tenant' }));
	expect(await view.findByText(/care-bus provisioning is off \(skipped\)/)).toBeTruthy();
	expect(view.queryByRole('button', { name: 'Retry care-bus provision' })).toBeNull();
});

it('offboard shows the care-bus suspend outcome', async () => {
	vi.mocked(all).mockResolvedValueOnce([acme]).mockResolvedValueOnce([]);
	vi.mocked(api).mockResolvedValue({
		tenant: { ...acme, archived_at: '2026-10-18T00:00:00Z' },
		carebus: {
			ok: true,
			skipped: false,
			steps: [{ step: 'suspend', status: 200, ok: true, error_code: null }],
		},
	});
	const { view, submit, input } = await openDialog();
	await fireEvent.input(input, { target: { value: 'Acme' } });
	await fireEvent.submit(submit.closest('form')!);
	expect(await view.findByText(/Offboarded “Acme”.*Suspended in care-bus\./)).toBeTruthy();
	expect(view.queryByRole('button', { name: 'Retry care-bus suspend' })).toBeNull();
});

it('offboard surfaces a failed care-bus suspend and retries it', async () => {
	vi.mocked(all).mockResolvedValueOnce([acme]).mockResolvedValue([]);
	vi.mocked(api).mockResolvedValueOnce({
		tenant: { ...acme, archived_at: '2026-10-18T00:00:00Z' },
		carebus: {
			ok: false,
			skipped: false,
			steps: [{ step: 'suspend', status: null, ok: false, error_code: 'carebus_unavailable' }],
		},
	});
	const { view, submit, input } = await openDialog();
	await fireEvent.input(input, { target: { value: 'Acme' } });
	await fireEvent.submit(submit.closest('form')!);
	expect(
		await view.findByText(/Offboarded “Acme”.*suspending it in care-bus failed \(carebus_unavailable\)/),
	).toBeTruthy();
	vi.mocked(api).mockResolvedValueOnce({
		ok: true,
		skipped: false,
		steps: [{ step: 'suspend', status: 200, ok: true, error_code: null }],
	});
	await fireEvent.click(view.getByRole('button', { name: 'Retry care-bus suspend' }));
	await waitFor(() =>
		expect(api).toHaveBeenCalledWith(`/api/tenants/${acme.id}/carebus-provision`, {}, { activeTenant: '' }),
	);
	expect(await view.findByText(/“Acme”: Suspended in care-bus\./)).toBeTruthy();
	expect(view.queryByRole('button', { name: 'Retry care-bus suspend' })).toBeNull();
});

it('offboard says so when care-bus is off', async () => {
	vi.mocked(all).mockResolvedValueOnce([acme]).mockResolvedValueOnce([]);
	vi.mocked(api).mockResolvedValue({
		tenant: { ...acme, archived_at: '2026-10-18T00:00:00Z' },
		carebus: { ok: false, skipped: true, steps: [] },
	});
	const { view, submit, input } = await openDialog();
	await fireEvent.input(input, { target: { value: 'Acme' } });
	await fireEvent.submit(submit.closest('form')!);
	expect(await view.findByText(/Offboarded “Acme”.*care-bus is off \(not suspended there\)\./)).toBeTruthy();
	expect(view.queryByRole('button', { name: /Retry care-bus/ })).toBeNull();
});

const gone = {
	id: '7a2b3c4d-5e6f-4a70-8b9c-0d1e2f3a4b5c',
	name: 'Gone Ltd',
	created_at: '2026-10-01T10:00:00Z',
	archived_at: '2026-10-18T09:30:00Z',
};

it('the archived view lists offboarded tenants and retries their care-bus suspend', async () => {
	// live view first, then the archived view (the API returns live + archived with include_archived)
	vi.mocked(all).mockResolvedValueOnce([acme]).mockResolvedValueOnce([acme, gone]);
	const view = render(Tenants);
	await view.findByText('Acme');
	expect(view.queryByText('Gone Ltd')).toBeNull();
	await fireEvent.click(view.getByRole('button', { name: 'Archived' }));
	await waitFor(() =>
		expect(all).toHaveBeenLastCalledWith('/api/tenants?include_archived=true', { activeTenant: '' }),
	);
	const row = (await view.findByText('Gone Ltd')).closest('tr')!;
	expect(row.textContent).toContain(gone.id);
	expect(row.textContent).toContain(new Date(gone.archived_at).toLocaleString());
	// only archived tenants are in this view, and none can be offboarded again
	expect(view.queryByText('Acme')).toBeNull();
	expect(view.queryByRole('button', { name: /^Offboard / })).toBeNull();

	vi.mocked(api).mockResolvedValueOnce({
		ok: false,
		skipped: false,
		steps: [{ step: 'suspend', status: 503, ok: false, error_code: 'carebus_unavailable' }],
	});
	await fireEvent.click(view.getByRole('button', { name: 'Retry care-bus suspend for Gone Ltd' }));
	await waitFor(() =>
		expect(api).toHaveBeenCalledWith(`/api/tenants/${gone.id}/carebus-provision`, {}, { activeTenant: '' }),
	);
	expect(await view.findByText(/suspending it in care-bus failed \(carebus_unavailable\)/)).toBeTruthy();

	vi.mocked(api).mockResolvedValueOnce({
		ok: true,
		skipped: false,
		steps: [{ step: 'suspend', status: 200, ok: true, error_code: null }],
	});
	await fireEvent.click(view.getByRole('button', { name: 'Retry care-bus suspend for Gone Ltd' }));
	expect(await view.findByText('Suspended in care-bus.')).toBeTruthy();
});

it('the archived view says so when there are none and switches back to live', async () => {
	vi.mocked(all).mockResolvedValueOnce([acme]).mockResolvedValueOnce([acme]).mockResolvedValueOnce([acme]);
	const view = render(Tenants);
	await view.findByText('Acme');
	await fireEvent.click(view.getByRole('button', { name: 'Archived' }));
	expect(await view.findByText('No archived tenants.')).toBeTruthy();
	await fireEvent.click(view.getByRole('button', { name: 'Live' }));
	expect(await view.findByText('Acme')).toBeTruthy();
	expect(all).toHaveBeenLastCalledWith('/api/tenants', { activeTenant: '' });
});

it('a retry failure in the archived view is shown on the row', async () => {
	vi.mocked(all).mockResolvedValueOnce([acme]).mockResolvedValueOnce([gone]);
	vi.mocked(api).mockRejectedValueOnce(
		new ApiError(
			'carebus_provision_disabled',
			'provisioning into care-bus is off (CAREBUS_PROVISION_ENABLED)',
		),
	);
	const view = render(Tenants);
	await view.findByText('Acme');
	await fireEvent.click(view.getByRole('button', { name: 'Archived' }));
	await fireEvent.click(await view.findByRole('button', { name: 'Retry care-bus suspend for Gone Ltd' }));
	const row = view.getByText('Gone Ltd').closest('tr')!;
	await waitFor(() => expect(row.textContent).toContain('CAREBUS_PROVISION_ENABLED'));
});
