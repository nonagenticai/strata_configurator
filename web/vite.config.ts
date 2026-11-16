import adapter from '@sveltejs/adapter-node';
import { sveltekit } from '@sveltejs/kit/vite';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

// SvelteKit 3 reads its configuration from here, not from `svelte.config.js`
// (that file is gone — kit 3 refuses to start while it exists). Options that
// are not SvelteKit's own are passed through to vite-plugin-svelte, which is
// why `preprocess` belongs here.
export default defineConfig({
	plugins: [
		sveltekit({
			preprocess: vitePreprocess(),
			adapter: adapter({ out: 'build' }),
			// Kit 3 dropped the built-in `$lib` alias. `#lib` is the subpath
			// import declared in package.json; the alias is what gives TypeScript
			// a `paths` entry for it, since kit 3's generated tsconfig has none.
			alias: { '#lib': 'src/lib' },
		}),
	],
	test: {
		environment: 'jsdom',
		include: ['tests/unit/**/*.test.ts'],
		setupFiles: [],
	},
	resolve: process.env.VITEST ? { conditions: ['browser'] } : undefined,
});
