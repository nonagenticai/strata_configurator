import Keycloak from 'keycloak-js';

let keycloak: Keycloak | undefined;
export async function initAuth(config: { keycloakUrl: string; realm: string; clientId: string }) {
	keycloak = new Keycloak({ url: config.keycloakUrl, realm: config.realm, clientId: config.clientId });
	await keycloak.init({ onLoad: 'login-required', pkceMethod: 'S256', checkLoginIframe: false });
	return keycloak.tokenParsed?.preferred_username || keycloak.tokenParsed?.sub || 'Signed in';
}
export async function getToken(force = false): Promise<string> {
	if (!keycloak) throw new Error('Please sign in to continue.');
	try {
		await keycloak.updateToken(force ? -1 : 30);
	} catch {
		await keycloak.login();
		throw new Error('Your session expired. Please sign in again.');
	}
	if (!keycloak.token) throw new Error('Please sign in to continue.');
	return keycloak.token;
}
export async function logout() {
	await keycloak?.logout({ redirectUri: window.location.origin });
}
