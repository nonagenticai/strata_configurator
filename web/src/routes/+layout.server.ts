import { env } from '$env/dynamic/public';

export function load() {
	return {
		config: {
			keycloakUrl: env.PUBLIC_KEYCLOAK_URL || 'https://auth.nonagentic.ai',
			realm: env.PUBLIC_KEYCLOAK_REALM || 'portal',
			clientId: env.PUBLIC_KEYCLOAK_CLIENT_ID || 'strata-configurator',
		},
	};
}
