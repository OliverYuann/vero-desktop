// OS keychain storage for the Supabase session (`desktop_secure_*`).
//
// macOS Keychain, Windows Credential Manager, Linux Secret Service (GNOME
// Keyring / KWallet), via the `keyring` crate. The web app's Supabase client
// uses this as its `storage` adapter inside the shell, so the refresh token
// never sits in the webview's localStorage.
//
// CHUNKING. Windows Credential Manager caps a credential blob at 2,560 bytes
// (CRED_MAX_CREDENTIAL_BLOB_SIZE), and a Supabase session JSON — access JWT +
// refresh token + user object — is routinely larger. So every value is stored
// as a small header entry plus N chunk entries:
//
//     <key>        "vero-chunks:v1:<n>"
//     <key>#0..n   ≤ 1,000 chars each (≤ 2,000 bytes as UTF-16)
//
// Same layout on every platform so behaviour does not diverge. Values are
// capped at 16 KB before chunking (`validate::SECURE_VALUE_MAX`), so n ≤ 17.

use keyring::{Entry, Error};

const SERVICE: &str = "com.verostocks.desktop";
const HEADER_PREFIX: &str = "vero-chunks:v1:";
const CHUNK_CHARS: usize = 1000;
const MAX_CHUNKS: usize = 64;

fn entry(user: &str) -> Result<Entry, String> {
    Entry::new(SERVICE, user).map_err(map_err)
}

fn map_err(e: Error) -> String {
    match e {
        Error::NoStorageAccess(_) | Error::PlatformFailure(_) => {
            format!("keychain unavailable: {e}")
        }
        other => format!("keychain error: {other}"),
    }
}

fn chunk_user(key: &str, i: usize) -> String {
    format!("{key}#{i}")
}

fn delete_if_present(user: &str) -> Result<(), String> {
    match entry(user)?.delete_credential() {
        Ok(()) | Err(Error::NoEntry) => Ok(()),
        Err(e) => Err(map_err(e)),
    }
}

fn header_count(key: &str) -> Result<Option<usize>, String> {
    match entry(key)?.get_password() {
        Ok(h) => Ok(h
            .strip_prefix(HEADER_PREFIX)
            .and_then(|n| n.parse().ok())
            .filter(|n| *n <= MAX_CHUNKS)),
        Err(Error::NoEntry) => Ok(None),
        Err(e) => Err(map_err(e)),
    }
}

pub fn get(key: &str) -> Result<Option<String>, String> {
    let Some(n) = header_count(key)? else {
        return Ok(None);
    };
    let mut out = String::new();
    for i in 0..n {
        match entry(&chunk_user(key, i))?.get_password() {
            Ok(part) => out.push_str(&part),
            // A torn write (crash between chunks): treat as absent so the
            // client re-authenticates instead of parsing half a session.
            Err(Error::NoEntry) => return Ok(None),
            Err(e) => return Err(map_err(e)),
        }
    }
    Ok(Some(out))
}

pub fn set(key: &str, value: &str) -> Result<(), String> {
    let old = header_count(key)?.unwrap_or(0);
    let chars: Vec<char> = value.chars().collect();
    let chunks: Vec<String> = if chars.is_empty() {
        vec![String::new()]
    } else {
        chars
            .chunks(CHUNK_CHARS)
            .map(|c| c.iter().collect())
            .collect()
    };
    for (i, part) in chunks.iter().enumerate() {
        entry(&chunk_user(key, i))?
            .set_password(part)
            .map_err(map_err)?;
    }
    entry(key)?
        .set_password(&format!("{HEADER_PREFIX}{}", chunks.len()))
        .map_err(map_err)?;
    for i in chunks.len()..old {
        delete_if_present(&chunk_user(key, i))?;
    }
    Ok(())
}

pub fn remove(key: &str) -> Result<(), String> {
    let n = header_count(key)?.unwrap_or(0);
    delete_if_present(key)?;
    for i in 0..n {
        delete_if_present(&chunk_user(key, i))?;
    }
    Ok(())
}

/// Is a keychain backend actually reachable? Reads a key that never exists:
/// `NoEntry` means the backend answered. Never prompts on macOS (a missing
/// item needs no authorisation). Linux without a Secret Service daemon — a
/// bare window manager, a container — reports unavailable here, and the web
/// side then keeps its default storage rather than losing the session.
pub fn probe() -> bool {
    match Entry::new(SERVICE, "vero-probe").and_then(|e| e.get_password()) {
        Ok(_) | Err(Error::NoEntry) => true,
        Err(e) => {
            eprintln!("[vero] keychain unavailable, securestore off: {e}");
            false
        }
    }
}
