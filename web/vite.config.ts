import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vitest/config';

export default defineConfig({
	plugins: [sveltekit()],
	test: {
		environment: 'jsdom',
		include: ['tests/unit/**/*.test.ts'],
		setupFiles: [],
	},
	resolve: process.env.VITEST ? { conditions: ['browser'] } : undefined,
});
