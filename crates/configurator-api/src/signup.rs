//! Early-access self-signup (W2.3): the intake guard, requester-typed text, the default workspace name,
//! Cloudflare Turnstile and the Telegram notices. After commonpractice
//! `middleware/signup-intake-auth.ts`, `utils/untrusted-text.ts`, `services/onboarding-service.ts`
//! (`defaultTenantNameFrom`), `services/turnstile.ts` and `services/signup-telegram.ts` (patterns
//! only, no shared code).
//!
//! Telegram is SEND ONLY and FIRE-AND-FORGET: a notice is sent from a detached task with a bounded
//! retry, every failure is logged and none is ever returned, so an outage cannot fail an intake or a
//! decision. The bot token is part of the API URL, so no transport error is ever formatted into a log.
//! No personal data is sent: ids and counts only (commonpractice `NewSignupNotice`).
use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::Caller;
use crate::config::SignupConfig;
use crate::error::ApiError;
use crate::service_token::form;

fn intake_forbidden(why: &str) -> ApiError {
    ApiError::Denied("signup_intake_forbidden", why.into())
}

/// The intake accepts only its dedicated client-credentials token: 403 `signup_intake_forbidden` for
/// a token that carries the super-admin role (an operator session never poses as the website), for a
/// client (`azp`) not in `SIGNUP_INTAKE_AZP`, or without the `SIGNUP_INTAKE_ROLE` client role.
pub fn require_intake_caller(cfg: &SignupConfig, caller: &Caller) -> Result<(), ApiError> {
    if caller.super_admin {
        return Err(intake_forbidden(
            "the signup intake accepts only its dedicated service-account token, never an operator session",
        ));
    }
    if !caller
        .azp
        .as_deref()
        .is_some_and(|a| cfg.intake_azp.iter().any(|x| x.as_str() == a))
    {
        return Err(intake_forbidden(
            "this client is not allowed to submit signups",
        ));
    }
    if cfg.intake_role.is_empty() || !caller.roles.contains(&cfg.intake_role) {
        return Err(intake_forbidden(&format!(
            "role '{}' is required to submit signups",
            cfg.intake_role
        )));
    }
    Ok(())
}

/// Unicode format characters (general category Cf): bidi embeddings, overrides and isolates, LRM/RLM,
/// zero-width characters, the BOM, the soft hyphen and the tag characters. Invisible by design, so
/// requester-typed text drops them.
fn is_format(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2 | 0x180E
        | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF
        | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
}

/// U+2028 LINE SEPARATOR and U+2029 PARAGRAPH SEPARATOR.
fn is_separator(c: char) -> bool {
    matches!(c, '\u{2028}' | '\u{2029}')
}

/// One printable line (commonpractice `singleLine`): control characters and line/paragraph separators
/// become spaces, format characters go, whitespace collapses to one space, and the ends are trimmed.
pub fn single_line(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut space = false;
    for c in v.chars() {
        if is_format(c) {
            continue;
        }
        if c.is_control() || is_separator(c) || c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out
}

/// Printable text keeping its line breaks (commonpractice `multiLine`): CRLF, CR and the line/paragraph
/// separators become `\n`, a tab a space, every other control and format character goes, and the ends
/// are trimmed.
pub fn multi_line(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' => out.push('\n'),
            '\t' => out.push(' '),
            c if is_separator(c) => out.push('\n'),
            c if c.is_control() || is_format(c) => {}
            c => out.push(c),
        }
    }
    out.trim().to_string()
}

fn too_long(field: &str, max: usize) -> ApiError {
    ApiError::BadRequest(
        "invalid_body",
        format!("{field} must be at most {max} characters"),
    )
}

/// A requester-typed one-line field: [`single_line`], then 1 to `max` characters (the raw input is
/// bounded at four times that first, as in commonpractice). Empty after stripping is refused.
pub fn single_line_field(field: &str, v: &str, max: usize) -> Result<String, ApiError> {
    if v.chars().count() > max * 4 {
        return Err(too_long(field, max));
    }
    let t = single_line(v);
    if t.is_empty() {
        return Err(ApiError::BadRequest(
            "invalid_body",
            format!("{field} must not be empty"),
        ));
    }
    if t.chars().count() > max {
        return Err(too_long(field, max));
    }
    Ok(t)
}

/// A requester-typed multi-line field: [`multi_line`], at most `max` characters; empty after
/// stripping is `None` (as if not given).
pub fn multi_line_field(field: &str, v: &str, max: usize) -> Result<Option<String>, ApiError> {
    if v.chars().count() > max * 4 {
        return Err(too_long(field, max));
    }
    let t = multi_line(v);
    if t.chars().count() > max {
        return Err(too_long(field, max));
    }
    Ok(Some(t).filter(|t| !t.is_empty()))
}

/// Public mailbox providers, which never name a customer (commonpractice `PUBLIC_EMAIL_DOMAINS`).
pub const PUBLIC_EMAIL_DOMAINS: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "outlook.com",
    "hotmail.com",
    "hotmail.co.uk",
    "live.com",
    "msn.com",
    "yahoo.com",
    "yahoo.co.uk",
    "ymail.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "aol.com",
    "proton.me",
    "protonmail.com",
    "pm.me",
    "gmx.com",
    "gmx.de",
    "gmx.net",
    "web.de",
    "mail.com",
    "zoho.com",
    "yandex.com",
    "yandex.ru",
    "hey.com",
    "fastmail.com",
    "tutanota.com",
    "tuta.io",
    "qq.com",
    "163.com",
    "ziggo.nl",
    "kpnmail.nl",
    "hetnet.nl",
    "planet.nl",
    "telenet.be",
    "orange.fr",
    "free.fr",
    "laposte.net",
    "t-online.de",
];

/// Second-level labels under a country TLD (`acme.co.uk`).
const SECOND_LEVEL_LABELS: [&str; 9] =
    ["co", "com", "org", "net", "ac", "gov", "edu", "ltd", "plc"];

/// `mail.acme.co.uk` → `acme` (commonpractice `organisationLabel`, no public-suffix list).
pub fn organisation_label(domain: &str) -> String {
    let lower = domain.trim().to_lowercase();
    let mut labels: Vec<&str> = lower.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() <= 1 {
        return labels.first().map(|l| l.to_string()).unwrap_or_default();
    }
    labels.pop();
    if labels.len() > 1
        && labels
            .last()
            .is_some_and(|l| SECOND_LEVEL_LABELS.contains(l))
    {
        labels.pop();
    }
    labels.last().map(|l| l.to_string()).unwrap_or_default()
}

/// A usable workspace name: trimmed, at most 120 characters, at least 2, with a letter or digit.
fn usable(v: Option<&str>) -> Option<String> {
    let t: String = v?.trim().chars().take(120).collect();
    let t = t.trim();
    (t.chars().count() >= 2 && t.chars().any(|c| c.is_ascii_alphanumeric())).then(|| t.to_string())
}

/// The workspace name an approval uses when none is given (commonpractice `defaultTenantNameFrom`):
/// the company; else the organisation of the email domain, unless it is a public mailbox provider; else
/// the requester's name; else the email's local part.
pub fn default_tenant_name(company: Option<&str>, email: &str, name: Option<&str>) -> String {
    if let Some(c) = usable(company) {
        return c;
    }
    let at = email.rfind('@');
    if let Some(i) = at {
        let domain = email[i + 1..].trim().to_lowercase();
        if !domain.is_empty() && !PUBLIC_EMAIL_DOMAINS.contains(&domain.as_str()) {
            let label = organisation_label(&domain);
            if let Some(l) = usable(Some(label.as_str())) {
                return l;
            }
        }
    }
    if let Some(n) = usable(name) {
        return n;
    }
    usable(Some(&email[..at.unwrap_or(0)])).unwrap_or_else(|| "workspace".into())
}

#[derive(Deserialize)]
struct SiteVerify {
    success: Option<bool>,
}

/// Attempts per Telegram message, and the backoff before each retry.
const MAX_ATTEMPTS: usize = 3;
const BACKOFF: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(4)];
/// Ceiling on honouring Telegram's `retry_after` (429).
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

/// The signup settings and an HTTP client for Turnstile and Telegram.
pub struct Signup {
    pub cfg: SignupConfig,
    http: reqwest::Client,
}

impl Signup {
    pub fn new(cfg: SignupConfig, http: reqwest::Client) -> Self {
        Self { cfg, http }
    }

    /// One line at boot: which parts are on (commonpractice `logTurnstileStatus` /
    /// `logSignupTelegramStatus`).
    pub fn log_status(&self) {
        if !self.cfg.enabled {
            tracing::info!("early_access_signup_disabled");
            return;
        }
        tracing::info!(
            turnstile = self.cfg.turnstile_secret.is_some(),
            telegram = self.cfg.telegram.is_some(),
            "early_access_signup_enabled"
        );
    }

    /// Verify a relayed Turnstile token (commonpractice `enforceTurnstile` with `requireToken: false`:
    /// the intake is a server, not a browser). No secret, or no token: skipped. siteverify unreachable or
    /// non-2xx → 503 `turnstile_unavailable`; a token it does not accept → 403 `turnstile_failed`.
    pub async fn verify_turnstile(&self, token: Option<&str>) -> Result<(), ApiError> {
        let Some(secret) = self.cfg.turnstile_secret.as_deref() else {
            return Ok(());
        };
        let Some(token) = token.map(str::trim).filter(|t| !t.is_empty()) else {
            return Ok(());
        };
        let unavailable = || {
            ApiError::Unavailable(
                "turnstile_unavailable",
                "sign-up verification is temporarily unavailable; try again shortly".into(),
            )
        };
        let resp = self
            .http
            .post(self.cfg.turnstile_verify_url.as_str())
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form(&[("secret", secret), ("response", token)]))
            .timeout(Duration::from_millis(self.cfg.turnstile_timeout_ms))
            .send()
            .await
            .map_err(|_| {
                tracing::warn!("turnstile_verify_unavailable");
                unavailable()
            })?;
        if !resp.status().is_success() {
            tracing::warn!(
                status = resp.status().as_u16(),
                "turnstile_verify_unavailable"
            );
            return Err(unavailable());
        }
        let body: SiteVerify = resp.json().await.map_err(|_| unavailable())?;
        if body.success != Some(true) {
            tracing::info!("turnstile_verify_rejected");
            return Err(ApiError::Denied(
                "turnstile_failed",
                "the verification challenge was not accepted".into(),
            ));
        }
        Ok(())
    }

    /// The UI page for one signup, with the decision to open; `None` without `CONFIGURATOR_PUBLIC_URL`.
    fn deep_link(&self, id: Uuid, action: &str) -> Option<String> {
        self.cfg
            .public_url
            .as_ref()
            .map(|u| format!("{u}/signups?signup={id}&action={action}"))
    }

    /// The `sendMessage` payload for a new pending signup: ids and counts only.
    pub fn new_signup_payload(
        &self,
        id: Uuid,
        source: &str,
        pending: Option<i64>,
    ) -> Option<Value> {
        let t = self.cfg.telegram.as_ref()?;
        let pending = pending.map_or_else(|| "—".to_string(), |n| n.to_string());
        let text = format!(
            "New signup request — review in the configurator's Signups page\n\n\
             Pending signups: {pending}\n\
             Source: {source}\n\
             Trusted via: website/chat intake (service account)\n\
             Signup id: {id}\n\n\
             Approve or Reject opens the Signups page (super-admin sign-in) with that decision \
             prefilled. Nothing changes until you confirm there."
        );
        let mut payload = json!({
            "chat_id": t.chat_id,
            "message_thread_id": t.thread_id,
            "text": text,
            "link_preview_options": {"is_disabled": true},
        });
        if let (Some(approve), Some(reject)) =
            (self.deep_link(id, "approve"), self.deep_link(id, "reject"))
        {
            payload["reply_markup"] = json!({"inline_keyboard": [[
                {"text": "Approve", "url": approve},
                {"text": "Reject", "url": reject},
            ]]});
        }
        Some(payload)
    }

    /// The `sendMessage` payload for a decision: the outcome and, for an approval, the tenant id.
    pub fn decision_payload(&self, id: Uuid, tenant: Option<Uuid>) -> Option<Value> {
        let t = self.cfg.telegram.as_ref()?;
        let text = match tenant {
            Some(tenant) => format!(
                "Strata configurator signup approved\n\nTenant id: {tenant}\n\
                 The requester was invited by email as its tenant admin.\nSignup id: {id}"
            ),
            None => format!("Strata configurator signup rejected\n\nSignup id: {id}"),
        };
        Some(json!({
            "chat_id": t.chat_id,
            "message_thread_id": t.thread_id,
            "text": text,
            "link_preview_options": {"is_disabled": true},
        }))
    }

    /// Announce a new pending signup (no-op while Telegram is off).
    pub fn notify_new(&self, id: Uuid, source: &str, pending: Option<i64>) {
        if let Some(p) = self.new_signup_payload(id, source, pending) {
            self.send(p, "new_signup");
        }
    }

    /// Announce an approval (`Some(tenant)`) or a rejection (no-op while Telegram is off).
    pub fn notify_decision(&self, id: Uuid, tenant: Option<Uuid>) {
        if let Some(p) = self.decision_payload(id, tenant) {
            self.send(p, "decision");
        }
    }

    /// Send `payload` from a detached task: at most [`MAX_ATTEMPTS`], `retry_after` honoured on 429
    /// (capped), no retry on any other 4xx. Never returns an error.
    fn send(&self, payload: Value, kind: &'static str) {
        let Some(t) = self.cfg.telegram.as_ref() else {
            return;
        };
        let url = format!("{}/bot{}/sendMessage", t.api_base, t.bot_token);
        let http = self.http.clone();
        tokio::spawn(async move {
            for attempt in 0..MAX_ATTEMPTS {
                let mut wait = None;
                match http.post(url.as_str()).json(&payload).send().await {
                    Ok(r) if r.status().is_success() => {
                        tracing::info!(kind, outcome = "ok", "signup_telegram_send");
                        return;
                    }
                    Ok(r) if r.status() == StatusCode::TOO_MANY_REQUESTS => {
                        let body: Option<Value> = r.json().await.ok();
                        wait = body
                            .as_ref()
                            .and_then(|b| b["parameters"]["retry_after"].as_u64())
                            .map(|s| Duration::from_secs(s).min(MAX_RETRY_AFTER));
                    }
                    Ok(r) if r.status().is_client_error() => {
                        tracing::warn!(
                            kind,
                            status = r.status().as_u16(),
                            outcome = "failed",
                            "signup_telegram_send refused; not retried"
                        );
                        return;
                    }
                    Ok(r) => {
                        tracing::warn!(
                            kind,
                            status = r.status().as_u16(),
                            attempt,
                            "signup_telegram_send failed"
                        );
                    }
                    // The error text carries the URL, which carries the bot token: never format it.
                    Err(e) => {
                        tracing::warn!(
                            kind,
                            timeout = e.is_timeout(),
                            attempt,
                            "signup_telegram_send failed"
                        );
                    }
                }
                let Some(backoff) = BACKOFF.get(attempt) else {
                    break;
                };
                tokio::time::sleep(wait.unwrap_or(*backoff)).await;
            }
            tracing::warn!(kind, outcome = "failed", "signup_telegram_send gave up");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_strips_controls_and_format_characters() {
        assert_eq!(single_line("  Ann\nEvil\r\n Corp  "), "Ann Evil Corp");
        assert_eq!(single_line("A\u{202E}nn\u{200B}"), "Ann");
        assert_eq!(single_line("a\u{2028}b\u{00A0}\u{3000}c\td"), "a b c d");
        assert_eq!(single_line("\u{200B}\n \u{FEFF}"), "");
        assert!(single_line_field("name", "\u{200B} \n", 160).is_err());
        assert!(single_line_field("name", &"x".repeat(161), 160).is_err());
        assert_eq!(
            single_line_field("name", &format!("{}\u{200B}", "x".repeat(160)), 160).unwrap(),
            "x".repeat(160)
        );
    }

    #[test]
    fn multi_line_keeps_line_breaks_only() {
        assert_eq!(
            multi_line(" Hi\r\nthere\rand\u{2029}more\tok\u{0007}\u{202E} "),
            "Hi\nthere\nand\nmore ok"
        );
        assert_eq!(
            multi_line_field("message", " \u{200B} ", 2000).unwrap(),
            None
        );
        assert!(multi_line_field("message", &"x".repeat(2001), 2000).is_err());
    }

    #[test]
    fn default_names_follow_commonpractice() {
        assert_eq!(
            default_tenant_name(Some("Globex"), "ann@globex.com", Some("Ann")),
            "Globex"
        );
        assert_eq!(
            default_tenant_name(None, "ann@mail.acme.co.uk", Some("Ann")),
            "acme"
        );
        assert_eq!(
            default_tenant_name(Some(" "), "ann.early@gmail.com", Some("Ann Early")),
            "Ann Early"
        );
        assert_eq!(
            default_tenant_name(None, "ann.early@gmail.com", Some("A")),
            "ann.early"
        );
        assert_eq!(default_tenant_name(None, "a@gmail.com", None), "workspace");
    }
}
