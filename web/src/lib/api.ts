import { getToken } from './auth';
import { session } from './session.svelte';

export interface Page<T> {
	items: T[];
	next: string | null;
}
export interface Origin {
	row_id: string;
	layer: Layer;
	op: string;
}
export interface Layer {
	id?: string;
	scope: string;
	industry?: string;
	department?: string;
	job?: string;
	rule?: string;
	dimension?: [string, string];
	tenant?: string;
}
export interface Row {
	id: string;
	kind: string;
	key: string;
	op: string;
	rev: number;
	fields: Record<string, unknown>;
	is_current: boolean;
	author: string;
	created_at: string;
}
export interface CompileError {
	code: string;
	key?: string;
	row_id?: string;
	message: string;
}
export interface EffectiveConfig {
	hash: string;
	entities: Record<string, Record<string, unknown>>;
	attributes: Record<string, Record<string, unknown>>;
	relations: Record<string, Record<string, unknown>>;
	job: Record<string, unknown> | null;
	provenance: Record<string, Origin[]>;
	warnings: { code: string; key: string }[];
}
export class ApiError extends Error {
	constructor(
		public code: string,
		message: string,
		public errors: CompileError[] = [],
	) {
		super(message);
	}
}
export async function api<T>(
	path: string,
	body?: unknown,
	options: { tenant?: string; signal?: AbortSignal } = {},
): Promise<T> {
	// Capture the tenant before awaiting token refresh; a picker change cannot retarget an in-flight request.
	const tenant = options.tenant ?? session.tenant;
	const send = async (force: boolean) => {
		const headers: Record<string, string> = { Authorization: `Bearer ${await getToken(force)}` };
		if (tenant) headers['X-Tenant-Id'] = tenant;
		if (body !== undefined) headers['Content-Type'] = 'application/json';
		return fetch(path, {
			method: body === undefined ? 'GET' : 'POST',
			headers,
			body: body === undefined ? undefined : JSON.stringify(body),
			signal: options.signal,
		});
	};
	let response = await send(false);
	if (response.status === 401) response = await send(true);
	const value = await response.json();
	if (!response.ok)
		throw new ApiError(
			value.error?.code || 'request_failed',
			value.error?.message || `Request failed (${response.status})`,
			value.errors || [],
		);
	return value as T;
}
export async function all<T>(
	path: string,
	options: { tenant?: string; signal?: AbortSignal } = {},
): Promise<T[]> {
	const items: T[] = [];
	let next: string | null = null;
	const tenant = options.tenant ?? session.tenant;
	do {
		const join = path.includes('?') ? '&' : '?';
		const page: Page<T> = await api(
			`${path}${join}limit=500${next ? `&before=${encodeURIComponent(next)}` : ''}`,
			undefined,
			{ ...options, tenant },
		);
		items.push(...page.items);
		next = page.next;
	} while (next);
	return items;
}
export function layerLabel(l: Layer): string {
	return [l.scope, l.industry, l.department, l.job, l.rule, l.dimension?.join('='), l.tenant ? 'tenant' : '']
		.filter(Boolean)
		.join('/');
}
