// Kit 3 removed `$env/dynamic/public`. This module is server-only
// (`+layout.server.ts`), so the process environment is read directly at
// request time by adapter-node — the same runtime semantics the old dynamic
// env module had, without introducing kit 3's `defineEnvVars` schema layer for
// three plain strings that already carry defaults.
export function load() {
	return {
		config: {
			keycloakUrl: process.env.PUBLIC_KEYCLOAK_URL || 'https://auth.nonagentic.ai',
			realm: process.env.PUBLIC_KEYCLOAK_REALM || 'portal',
			clientId: process.env.PUBLIC_KEYCLOAK_CLIENT_ID || 'strata-configurator',
		},
	};
}
