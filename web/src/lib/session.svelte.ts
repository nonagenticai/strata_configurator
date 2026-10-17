export const session = $state({
	ready: false,
	tenant: '',
	superAdmin: false,
	name: '',
	tenants: [] as { id: string; name: string; role: string }[],
});
