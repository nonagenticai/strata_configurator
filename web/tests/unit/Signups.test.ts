import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
vi.mock('../../src/lib/api', async (original) => ({
	...(await original<typeof import('../../src/lib/api')>()),
	all: vi.fn(),
	api: vi.fn(),
}));
import Signups from '../../src/routes/signups/+page.svelte';
import { all, api, ApiError } from '../../src/lib/api';
import { session } from '../../src/lib/session.svelte';

const pending = {
	id: 'sig-1',
	email: 'ann@globex.com',
	name: 'Ann Early',
	company: 'Globex',
	message: 'We are keen',
	source: 'web',
	status: 'pending',
	created_at: '2026-10-17T10:00:00Z',
	suggested_tenant_name: 'Globex',
};

afterEach(() => {
	cleanup();
	window.history.replaceState({}, '', '/');
});
beforeEach(() => {
	vi.mocked(all).mockReset();
	vi.mocked(api).mockReset();
	session.superAdmin = true;
	session.activeTenant = 'tenant-a';
	// jsdom has no modal dialogs.
	HTMLDialogElement.prototype.showModal = function (this: HTMLDialogElement) {
		this.open = true;
	};
});

it('lists pending signups at platform scope and approves with the edited workspace name', async () => {
	vi.mocked(all).mockResolvedValueOnce([pending]).mockResolvedValueOnce([]);
	vi.mocked(api).mockResolvedValue({ tenant: { id: 't-1', name: 'Globex Europe' } });
	const view = render(Signups);
	expect(await view.findByText('ann@globex.com')).toBeTruthy();
	expect(all).toHaveBeenCalledWith('/api/signup-approvals?status=pending', { activeTenant: '' });

	await fireEvent.click(view.getByRole('button', { name: 'Approve' }));
	const name = view.getByLabelText('Workspace name') as HTMLInputElement;
	expect(name.value).toBe('Globex');
	await fireEvent.input(name, { target: { value: 'Globex Europe' } });
	expect(api).not.toHaveBeenCalled();
	await fireEvent.submit(view.getByRole('button', { name: 'Approve and create workspace' }).closest('form')!);
	await waitFor(() =>
		expect(api).toHaveBeenCalledWith(
			'/api/signup-approvals/sig-1/approve',
			{ tenantName: 'Globex Europe' },
			{ activeTenant: '' },
		),
	);
	expect(await view.findByText(/workspace “Globex Europe” created/)).toBeTruthy();
	expect(await view.findByText('No pending signups.')).toBeTruthy();
});

it('rejects after confirmation', async () => {
	vi.mocked(all).mockResolvedValueOnce([pending]).mockResolvedValueOnce([]);
	vi.mocked(api).mockResolvedValue({});
	const view = render(Signups);
	await view.findByText('ann@globex.com');
	await fireEvent.click(view.getByRole('button', { name: 'Reject' }));
	expect(api).not.toHaveBeenCalled();
	await fireEvent.submit(view.getByRole('button', { name: 'Reject signup' }).closest('form')!);
	await waitFor(() =>
		expect(api).toHaveBeenCalledWith('/api/signup-approvals/sig-1/reject', {}, { activeTenant: '' }),
	);
});

it('opens the dialog a Telegram link names, prefilled but unconfirmed', async () => {
	window.history.replaceState({}, '', '/signups?signup=sig-1&action=approve');
	vi.mocked(all).mockResolvedValue([pending]);
	const view = render(Signups);
	expect(await view.findByRole('button', { name: 'Approve and create workspace' })).toBeTruthy();
	expect((view.getByLabelText('Workspace name') as HTMLInputElement).value).toBe('Globex');
	expect(api).not.toHaveBeenCalled();
});

it('says so when early access is switched off', async () => {
	vi.mocked(all).mockRejectedValue(new ApiError('not_found', 'route not found'));
	const view = render(Signups);
	expect(await view.findByText('Early access is off')).toBeTruthy();
});

it('shows a decision error and keeps the signup listed', async () => {
	vi.mocked(all).mockResolvedValue([pending]);
	vi.mocked(api).mockRejectedValue(new ApiError('approval_not_pending', 'signup sig-1 is no longer pending'));
	const view = render(Signups);
	await view.findByText('ann@globex.com');
	await fireEvent.click(view.getByRole('button', { name: 'Approve' }));
	await fireEvent.submit(view.getByRole('button', { name: 'Approve and create workspace' }).closest('form')!);
	expect(await view.findByRole('alert')).toBeTruthy();
	expect(view.getByRole('alert').textContent).toContain('no longer pending');
});
