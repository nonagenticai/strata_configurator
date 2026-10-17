import { afterEach, expect, it } from 'vitest';
import { cleanup, render } from '@testing-library/svelte';
import Provenance from '../../src/lib/Provenance.svelte';
afterEach(cleanup);
it('renders origins in layer order with op labels and row links', () => {
	const { getAllByRole } = render(Provenance, {
		origins: [
			{ row_id: 'one', layer: { scope: 'base' }, op: 'add' },
			{ row_id: 'two', layer: { scope: 'industry', industry: 'retail' }, op: 'patch' },
			{ row_id: 'three', layer: { scope: 'tenant', tenant: 'tenant-a' }, op: 'patch' },
		],
	});
	const links = getAllByRole('link');
	expect(links.map((l) => l.textContent?.trim())).toEqual([
		'base add',
		'industry/retail patch',
		'tenant patch',
	]);
	expect(links[1]?.getAttribute('href')).toBe('/layers?row=two');
});
