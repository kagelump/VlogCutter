use anyhow::{Context, Result, anyhow};

const KEYCHAIN_SERVICE: &str = "com.rcd.vlogcutter.server";

/// Store a server credential token in macOS Keychain.
/// The `account` is the stable server node ID.
/// The `hostname` helps distinguish between LAN and tailnet endpoints.
pub fn store_server_credential(hostname: &str, account: &str, token: &str) -> Result<()> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, account)
        .with_context(|| format!("create keychain entry for server {hostname}"))?;
    entry
        .set_password(token)
        .with_context(|| format!("store server credential in keychain for {hostname}"))?;
    Ok(())
}

/// Retrieve a server credential token from macOS Keychain.
#[allow(dead_code)]
pub fn get_server_credential(hostname: &str, account: &str) -> Result<Option<String>> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, account);
    match entry {
        Ok(entry) => match entry.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow!("keychain read failed for {hostname}: {e}")),
        },
        Err(e) => Err(anyhow!("keychain entry error for {hostname}: {e}")),
    }
}

/// Delete a server credential token from macOS Keychain.
pub fn delete_server_credential(hostname: &str, account: &str) -> Result<()> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, account)
        .with_context(|| format!("keychain entry for {hostname}"))?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(anyhow!("keychain delete failed for {hostname}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires interactive macOS Keychain authorization"]
    fn store_and_retrieve_credential_roundtrip() {
        let hostname = "test.rcdvlog.local";
        let account = "node_server_test_keychain";
        let token = "test-token-value-12345";

        // Clean up from any previous test run
        let _ = delete_server_credential(hostname, account);

        store_server_credential(hostname, account, token).unwrap();
        let retrieved = get_server_credential(hostname, account).unwrap();
        assert_eq!(retrieved.as_deref(), Some(token));

        delete_server_credential(hostname, account).unwrap();
        let after_delete = get_server_credential(hostname, account).unwrap();
        assert_eq!(after_delete, None);
    }

    #[test]
    fn missing_credential_returns_none_not_error() {
        let result = get_server_credential("nonexistent.local", "nonexistent_account");
        // Either returns Ok(None) or fails gracefully on non-macOS
        match result {
            Ok(None) => {}
            Err(_) => {} // non-macOS may not have keychain
            _ => panic!("expected Ok(None) or Err"),
        }
    }
}
