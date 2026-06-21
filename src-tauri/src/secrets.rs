//! Secrets — API keys live in the OS keychain (Windows Credential Manager via `keyring`), never
//! in a config file or DB (DESIGN.md §9.2).

#![allow(dead_code)]

use crate::state::{AppError, AppResult};

const SERVICE: &str = "dev.tianji.app";

pub fn set_api_key(provider: &str, key: &str) -> AppResult<()> {
    let entry = keyring::Entry::new(SERVICE, provider).map_err(to_app)?;
    entry.set_password(key).map_err(to_app)
}

pub fn get_api_key(provider: &str) -> AppResult<Option<String>> {
    let entry = keyring::Entry::new(SERVICE, provider).map_err(to_app)?;
    match entry.get_password() {
        Ok(k) => Ok(Some(k)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(to_app(e)),
    }
}

fn to_app(e: keyring::Error) -> AppError {
    AppError::Message(format!("keychain error: {e}"))
}
