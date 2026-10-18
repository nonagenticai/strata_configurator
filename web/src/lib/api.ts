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
	/** Absent when the job has no content (or no job is selected). */
	job_content?: JobContent;
	provenance: Record<string, Origin[]>;
	warnings: { code: string; key: string }[];
}
/** The job's content beyond the ontology, keyed by overlay row key (`name@version` for procedures). */
export interface JobContent {
	context_spaces: Record<string, Record<string, unknown>>;
	procedures: Record<string, Record<string, unknown>>;
	connectors: Record<string, Record<string, unknown>>;
	knowledge_stores: Record<string, Record<string, unknown>>;
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
	options: { activeTenant?: string; signal?: AbortSignal; method?: 'GET' | 'POST' | 'PUT' | 'DELETE' } = {},
): Promise<T> {
	// Capture the selection before awaiting token refresh; a switcher change cannot retarget an in-flight request.
	// Only a super-admin selects a tenant; everyone else is scoped to their home tenant by the server.
	const activeTenant = session.superAdmin ? (options.activeTenant ?? session.activeTenant) : '';
	const send = async (force: boolean) => {
		const headers: Record<string, string> = { Authorization: `Bearer ${await getToken(force)}` };
		if (activeTenant) headers['X-Active-Tenant-Id'] = activeTenant;
		if (body !== undefined) headers['Content-Type'] = 'application/json';
		return fetch(path, {
			method: options.method ?? (body === undefined ? 'GET' : 'POST'),
			headers,
			body: body === undefined ? undefined : JSON.stringify(body),
			signal: options.signal,
		});
	};
	let response = await send(false);
	if (response.status === 401) response = await send(true);
	// 204 (a DELETE) has no body.
	const value = response.status === 204 ? undefined : await response.json();
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
	options: { activeTenant?: string; signal?: AbortSignal } = {},
): Promise<T[]> {
	const items: T[] = [];
	let next: string | null = null;
	const activeTenant = options.activeTenant ?? session.activeTenant;
	do {
		const join = path.includes('?') ? '&' : '?';
		const page: Page<T> = await api(
			`${path}${join}limit=500${next ? `&before=${encodeURIComponent(next)}` : ''}`,
			undefined,
			{ ...options, activeTenant },
		);
		items.push(...page.items);
		next = page.next;
	} while (next);
	return items;
}
export function layerLabel(l: Layer): string {
	return [
		l.scope,
		l.industry,
		l.department,
		l.job,
		l.rule,
		l.dimension?.join('='),
		l.tenant && l.scope !== 'tenant' ? 'tenant' : '',
	]
		.filter(Boolean)
		.join('/');
}
/** A release target (`/api/targets`): one job of the tenant with its stored selection. */
export interface Target {
	id: string;
	job: string;
	selection: Record<string, unknown>;
	created_by: string;
	created_at: string;
}
/** A built release (`cfg-YYYY-MM-DD.N`) of a target. */
export interface Release {
	id: string;
	target: string;
	release_id: string;
	hash: string;
	created_by: string;
	created_at: string;
}
/** One care-bus call made while pushing a release. */
export interface PushStep {
	id: number;
	step: 'tenant' | 'job' | 'release';
	status: number | null;
	ok: boolean;
	response: Record<string, unknown> | null;
	error_code: string | null;
	pushed_at: string;
}
/** A care-bus release conflict: a tenant refinement the new base no longer fits. */
export interface Conflict {
	id: number;
	kind: string;
	object: string;
	key: Record<string, unknown>;
	tenant_row: string;
}
export interface Drift {
	status: 'in_sync' | 'drift' | 'never_pushed' | 'carebus_has_no_release' | 'carebus_unavailable';
	carebus: { release_id: string; hash: string; applied_at: string; conflicts: Conflict[] } | null;
	last_pushed: { release_id: string; hash: string; pushed_at: string } | null;
	latest_built: { release_id: string; hash: string } | null;
	config_changed: boolean;
}
/** How provisioning a tenant into care-bus went (`carebus` on create and import), or suspending it
 * there on offboard (one step, `suspend`). */
export interface CarebusProvision {
	ok: boolean;
	skipped: boolean;
	steps: { step: string; status: number | null; ok: boolean; error_code: string | null }[];
}
/** A bundle violation (422 `bundle_incomplete` / `bundle_invalid`). */
export interface Violation {
	path: string;
	message: string;
}
