export type TenantRole = 'tenant_admin' | 'tenant_user';
export interface Workspace {
	id: string;
	name: string;
	role: TenantRole;
}
export const session = $state({
	ready: false,
	name: '',
	superAdmin: false,
	/** The caller's home tenant from /api/me; null when they have no workspace. */
	workspace: null as Workspace | null,
	/** Super-admin only: tenant selected in the switcher ('' = Platform standards). */
	activeTenant: '',
	/** Super-admin only: every tenant, for the switcher. */
	tenants: [] as { id: string; name: string }[],
});
export function roleLabel(role: TenantRole | string): string {
	return role === 'tenant_admin' ? 'Tenant admin' : role === 'tenant_user' ? 'Tenant user' : role;
}
