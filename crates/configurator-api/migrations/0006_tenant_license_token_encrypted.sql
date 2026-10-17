-- The tenant licence token is stored encrypted (AES-256-GCM under LICENSE_TOKEN_ENC_KEY, see
-- src/license_token.rs), as commonpractice stores `tenants.license_token_encrypted`; the owner-only
-- grant of 0004 stays as a second layer. Stored shape: 'v1:' || base64(nonce 12 || ciphertext || tag 16).
--
-- Forward-only, no plaintext fallback: issuance shipped dark, so no row can exist yet. Should one
-- exist anyway, this migration stops (and the service with it) rather than leave a token in clear or
-- one the gate could not read: revoke that licence and delete the row, then restart.

DO $$
DECLARE
    clear_rows bigint;
BEGIN
    SELECT count(*) INTO clear_rows FROM tenant_license WHERE token NOT LIKE 'v1:%';
    IF clear_rows > 0 THEN
        RAISE EXCEPTION 'tenant_license holds % plaintext licence token(s); revoke and delete them before migrating', clear_rows;
    END IF;
END
$$;

ALTER TABLE tenant_license RENAME COLUMN token TO token_encrypted;
ALTER TABLE tenant_license
    ADD CONSTRAINT tenant_license_token_encrypted_v1 CHECK (token_encrypted LIKE 'v1:%');
