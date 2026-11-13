//! Email-code accounts, opaque login sessions and replaceable tenant pairing
//! codes (D049). Plain login/session secrets are never stored in the database.

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::db::{self, DbError};
use crate::pairing_crypto::PairingCipher;
use crate::secrets::{self, Secret, sha256_hex};

pub const CODE_TTL_MS: i64 = 10 * 60 * 1000;
pub const SESSION_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;
pub const SEND_INTERVAL_MS: i64 = 60 * 1000;
pub const SEND_WINDOW_MS: i64 = 60 * 60 * 1000;
pub const MAX_SEND_PER_WINDOW: i64 = 5;
pub const MAX_ATTEMPTS: i64 = 8;
pub const MAX_SESSIONS_PER_ACCOUNT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountError {
    BadEmail,
    BadCode,
    ExpiredCode,
    TooManyAttempts,
    TooManyRequests,
    Unavailable,
}

impl From<rusqlite::Error> for AccountError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Unavailable
    }
}

impl From<DbError> for AccountError {
    fn from(_: DbError) -> Self {
        Self::Unavailable
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub email: String,
    pub tenant_id: String,
}

pub struct Login {
    pub session: Secret,
    pub account: Account,
}

pub fn normalize_email(input: &str) -> Option<String> {
    let email = input.trim().to_ascii_lowercase();
    let (local, domain) = email.split_once('@')?;
    let domain = domain.strip_suffix('.').unwrap_or(domain);
    let valid = !local.is_empty()
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local.len() <= 64
        && email.len() <= 254
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains("..")
        && email.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'.' | b'-' | b'_' | b'+')
        });
    valid.then_some(email)
}

fn random_digits() -> String {
    let number = getrandom::u32().expect("operating system random source unavailable") % 1_000_000;
    format!("{number:06}")
}

/// Stages or refreshes a code. Call the mailer only after this succeeds; a
/// failed send does not consume an older valid code.
pub fn stage_code(conn: &Connection, email: &str, now: i64) -> Result<Secret, AccountError> {
    let email = normalize_email(email).ok_or(AccountError::BadEmail)?;
    if let Some((last_sent, sent_count, window_started)) = conn
        .query_row(
            "SELECT last_sent_at, sent_count, window_started_at FROM email_challenges WHERE email = ?1",
            params![email],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
        )
        .optional()?
    {
        if now.saturating_sub(last_sent) < SEND_INTERVAL_MS {
            return Err(AccountError::TooManyRequests);
        }
        if now.saturating_sub(window_started) < SEND_WINDOW_MS && sent_count >= MAX_SEND_PER_WINDOW {
            return Err(AccountError::TooManyRequests);
        }
    }
    let code = Secret::from_digits(random_digits());
    let fresh_window = conn
        .query_row(
            "SELECT window_started_at FROM email_challenges WHERE email = ?1 AND ?2 - window_started_at >= ?3",
            params![email, now, SEND_WINDOW_MS],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some();
    conn.execute(
        "INSERT INTO email_challenges
          (email, code_hash, created_at, expires_at, attempts, sent_count, window_started_at, last_sent_at, consumed_at)
         VALUES (?1, ?2, ?3, ?4, 0, 1, ?3, ?3, NULL)
         ON CONFLICT(email) DO UPDATE SET code_hash = excluded.code_hash,
           created_at = excluded.created_at, expires_at = excluded.expires_at,
           attempts = 0, consumed_at = NULL, last_sent_at = excluded.last_sent_at,
           sent_count = CASE WHEN ?5 THEN 1 ELSE email_challenges.sent_count + 1 END,
           window_started_at = CASE WHEN ?5 THEN excluded.window_started_at ELSE email_challenges.window_started_at END",
        params![email, sha256_hex(code.expose()), now, now + CODE_TTL_MS, fresh_window],
    )?;
    Ok(code)
}

/// Deletes a staged code after its email was accepted for delivery. No code
/// remains usable if delivery failed before this point.
pub fn mark_sent(conn: &Connection, email: &str) -> Result<(), AccountError> {
    let email = normalize_email(email).ok_or(AccountError::BadEmail)?;
    conn.execute(
        "UPDATE email_challenges SET last_sent_at = created_at WHERE email = ?1 AND consumed_at IS NULL",
        params![email],
    )?;
    Ok(())
}

fn create_account(tx: &Transaction<'_>, email: &str, now: i64) -> Result<Account, AccountError> {
    // The slug of a personal space uses a-z and digits only; `acc_` is not a
    // valid tenant short name.
    let unique = sha256_hex(&format!("{email}:{now}:{}", secrets::new_token_id()));
    let id = format!("acc_{}", &unique[..24]);
    let tenant_id = db::create_tenant(tx, &format!("u{}", &unique[..24]), "我的空间", now)?;
    tx.execute(
        "INSERT INTO accounts (id, email, tenant_id, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, email, tenant_id, now],
    )?;
    Ok(Account {
        id,
        email: email.to_owned(),
        tenant_id,
    })
}

/// Every account owns exactly one personal space. The email challenge and
/// account/tenant/session commit together; no other tenant id is accepted.
pub fn login(
    conn: &mut Connection,
    email: &str,
    code: &str,
    now: i64,
) -> Result<Login, AccountError> {
    let email = normalize_email(email).ok_or(AccountError::BadEmail)?;
    let code = code.trim();
    if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AccountError::BadCode);
    }
    let tx = conn.transaction()?;
    let Some((hash, expires, attempts)) = tx
        .query_row(
            "SELECT code_hash, expires_at, attempts FROM email_challenges WHERE email = ?1 AND consumed_at IS NULL",
            params![email],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
        )
        .optional()?
    else {
        return Err(AccountError::BadCode);
    };
    if attempts >= MAX_ATTEMPTS {
        return Err(AccountError::TooManyAttempts);
    }
    if now >= expires {
        tx.execute(
            "DELETE FROM email_challenges WHERE email = ?1",
            params![email],
        )?;
        tx.commit()?;
        return Err(AccountError::ExpiredCode);
    }
    if hash != sha256_hex(code) {
        let next = attempts + 1;
        if next >= MAX_ATTEMPTS {
            tx.execute(
                "DELETE FROM email_challenges WHERE email = ?1",
                params![email],
            )?;
        } else {
            tx.execute(
                "UPDATE email_challenges SET attempts = ?2 WHERE email = ?1",
                params![email, next],
            )?;
        }
        tx.commit()?;
        return Err(AccountError::BadCode);
    }
    tx.execute(
        "UPDATE email_challenges SET consumed_at = ?2 WHERE email = ?1",
        params![email, now],
    )?;
    let account = tx
        .query_row(
            "SELECT id, email, tenant_id FROM accounts WHERE email = ?1",
            params![email],
            |row| {
                Ok(Account {
                    id: row.get(0)?,
                    email: row.get(1)?,
                    tenant_id: row.get(2)?,
                })
            },
        )
        .optional()?
        .map_or_else(|| create_account(&tx, &email, now), Ok)?;
    let session = secrets::new_session();
    tx.execute(
        "INSERT INTO account_sessions (token_hash, account_id, tenant_id, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            sha256_hex(session.expose()),
            account.id,
            account.tenant_id,
            now,
            now + SESSION_TTL_MS
        ],
    )?;
    tx.commit()?;
    Ok(Login { session, account })
}

fn open_session(tx: &Transaction<'_>, account: &Account, now: i64) -> Result<Secret, AccountError> {
    tx.execute(
        "DELETE FROM account_sessions WHERE account_id = ?1 AND (expires_at <= ?2 OR revoked_at IS NOT NULL)",
        params![account.id, now],
    )?;
    let active: usize = tx.query_row(
        "SELECT COUNT(*) FROM account_sessions WHERE account_id = ?1 AND expires_at > ?2 AND revoked_at IS NULL",
        params![account.id, now],
        |row| row.get(0),
    )?;
    if active >= MAX_SESSIONS_PER_ACCOUNT {
        tx.execute(
            "DELETE FROM account_sessions WHERE account_id = ?1 AND created_at = (SELECT MIN(created_at) FROM account_sessions WHERE account_id = ?1 AND revoked_at IS NULL)",
            params![account.id],
        )?;
    }
    let session = secrets::new_session();
    tx.execute(
        "INSERT INTO account_sessions (token_hash, account_id, tenant_id, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            sha256_hex(session.expose()),
            account.id,
            account.tenant_id,
            now,
            now + SESSION_TTL_MS
        ],
    )?;
    Ok(session)
}

/// Signs in through a third-party identity. The provider's own user id picks
/// the account; only its verified email may join an existing account.
pub fn login_with_identity(
    conn: &mut Connection,
    provider: &str,
    subject: &str,
    email: &str,
    now: i64,
) -> Result<Login, AccountError> {
    if provider.is_empty() || subject.is_empty() {
        return Err(AccountError::Unavailable);
    }
    let email = normalize_email(email).ok_or(AccountError::BadEmail)?;
    let tx = conn.transaction()?;
    let read = |row: &rusqlite::Row<'_>| {
        Ok(Account {
            id: row.get(0)?,
            email: row.get(1)?,
            tenant_id: row.get(2)?,
        })
    };
    let linked = tx
        .query_row(
            "SELECT a.id, a.email, a.tenant_id FROM account_identities i
             JOIN accounts a ON a.id = i.account_id
             WHERE i.provider = ?1 AND i.subject = ?2",
            params![provider, subject],
            read,
        )
        .optional()?;
    let account = match linked {
        Some(account) => {
            tx.execute(
                "UPDATE account_identities SET last_login_at = ?3, email = ?4
                 WHERE provider = ?1 AND subject = ?2",
                params![provider, subject, now, email],
            )?;
            account
        }
        None => {
            let by_email = tx
                .query_row(
                    "SELECT id, email, tenant_id FROM accounts WHERE email = ?1",
                    params![email],
                    read,
                )
                .optional()?;
            let account = match by_email {
                Some(account) => account,
                None => create_account(&tx, &email, now)?,
            };
            tx.execute(
                "INSERT INTO account_identities
                   (provider, subject, account_id, email, created_at, last_login_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![provider, subject, account.id, email, now],
            )?;
            account
        }
    };
    let session = open_session(&tx, &account, now)?;
    tx.commit()?;
    Ok(Login { session, account })
}

pub fn authenticate(
    conn: &Connection,
    session: &str,
    now: i64,
) -> Result<Option<Account>, AccountError> {
    if !session.starts_with("tls_") || session.len() > 128 {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT a.id, a.email, a.tenant_id FROM account_sessions s
         JOIN accounts a ON a.id = s.account_id AND a.tenant_id = s.tenant_id
         JOIN tenants t ON t.id = a.tenant_id AND t.revoked_at IS NULL
         WHERE s.token_hash = ?1 AND s.revoked_at IS NULL AND s.expires_at > ?2",
            params![sha256_hex(session), now],
            |row| {
                Ok(Account {
                    id: row.get(0)?,
                    email: row.get(1)?,
                    tenant_id: row.get(2)?,
                })
            },
        )
        .optional()?)
}

pub fn logout(conn: &Connection, session: &str, now: i64) -> Result<(), AccountError> {
    conn.execute(
        "UPDATE account_sessions SET revoked_at = ?2 WHERE token_hash = ?1 AND revoked_at IS NULL",
        params![sha256_hex(session), now],
    )?;
    Ok(())
}

pub fn delete_challenge(conn: &Connection, email: &str) -> Result<(), AccountError> {
    conn.execute(
        "DELETE FROM email_challenges WHERE email = ?1",
        params![email],
    )?;
    Ok(())
}

/// Returns the current persistent code or creates one on first use. Rotation
/// changes the code atomically, so an old code can never be used again.
pub fn tenant_code(
    conn: &mut Connection,
    tenant_id: &str,
    rotate: bool,
    now: i64,
    cipher: &PairingCipher,
) -> Result<Secret, AccountError> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM tenants WHERE id = ?1 AND revoked_at IS NULL",
            params![tenant_id],
            |_| Ok(()),
        )
        .optional()?;
    if exists.is_none() {
        return Err(AccountError::Unavailable);
    }
    let code = if rotate {
        let code = secrets::new_pair_code();
        conn.execute(
            "INSERT INTO tenant_pair_secrets (tenant_id, code_hash, encrypted_code, created_at, rotated_at) VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(tenant_id) DO UPDATE SET code_hash = excluded.code_hash, encrypted_code = excluded.encrypted_code, rotated_at = excluded.rotated_at",
            params![tenant_id, secrets::pair_code_hash(&code), cipher.encrypt(secrets::format_pair_code(&code).expose()), now],
        )?;
        code
    } else if let Some(encrypted) = conn
        .query_row(
            "SELECT encrypted_code FROM tenant_pair_secrets WHERE tenant_id = ?1",
            params![tenant_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        return cipher
            .decrypt(&encrypted)
            .map(Secret::from_digits)
            .ok_or(AccountError::Unavailable);
    } else {
        let code = secrets::new_pair_code();
        conn.execute(
            "INSERT INTO tenant_pair_secrets (tenant_id, code_hash, encrypted_code, created_at, rotated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![tenant_id, secrets::pair_code_hash(&code), cipher.encrypt(secrets::format_pair_code(&code).expose()), now],
        )?;
        code
    };
    Ok(secrets::format_pair_code(&code))
}

pub fn create_device_for_account(
    conn: &mut Connection,
    tenant_id: &str,
    now: i64,
    device: &db::NewDevice<'_>,
) -> Result<db::ClaimedDevice, AccountError> {
    let tx = conn.transaction()?;
    let exists = tx
        .query_row(
            "SELECT 1 FROM tenants WHERE id = ?1 AND revoked_at IS NULL",
            params![tenant_id],
            |_| Ok(()),
        )
        .optional()?;
    if exists.is_none() {
        return Err(AccountError::Unavailable);
    }
    let device_id = secrets::new_device_id();
    tx.execute(
        "INSERT INTO devices (id, display_name, platform, form_factor, created_at, tenant_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![device_id, device.display_name, device.platform, device.form_factor, now, tenant_id],
    )?;
    let ingest =
        db::insert_device_token(&tx, tenant_id, &device_id, secrets::TokenKind::Ingest, now)?;
    let read = db::insert_device_token(&tx, tenant_id, &device_id, secrets::TokenKind::Read, now)?;
    tx.commit()?;
    Ok(db::ClaimedDevice {
        device_id,
        ingest_token: ingest,
        read_token: read,
    })
}

pub fn claim_tenant_code(
    conn: &mut Connection,
    code_hash: &str,
    now: i64,
    device: &db::NewDevice<'_>,
) -> Result<Option<db::ClaimedDevice>, AccountError> {
    let tx = conn.transaction()?;
    let Some(tenant_id) = tx.query_row(
        "SELECT s.tenant_id FROM tenant_pair_secrets s JOIN tenants t ON t.id = s.tenant_id AND t.revoked_at IS NULL WHERE s.code_hash = ?1",
        params![code_hash], |row| row.get::<_, String>(0),
    ).optional()? else {
        return Ok(None);
    };
    let device_id = secrets::new_device_id();
    tx.execute(
        "INSERT INTO devices (id, display_name, platform, form_factor, created_at, tenant_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![device_id, device.display_name, device.platform, device.form_factor, now, tenant_id],
    )?;
    let ingest =
        db::insert_device_token(&tx, &tenant_id, &device_id, secrets::TokenKind::Ingest, now)?;
    let read = db::insert_device_token(&tx, &tenant_id, &device_id, secrets::TokenKind::Read, now)?;
    tx.commit()?;
    Ok(Some(db::ClaimedDevice {
        device_id,
        ingest_token: ingest,
        read_token: read,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emails_are_normalized_and_malformed_ones_refused() {
        assert_eq!(
            normalize_email(" A+B@Example.COM ").as_deref(),
            Some("a+b@example.com")
        );
        for value in [
            "",
            "a",
            "@example.com",
            "a@localhost",
            "a..b@example.com",
            ".a@example.com",
            "a b@example.com",
        ] {
            assert!(normalize_email(value).is_none(), "{value}");
        }
    }
}
