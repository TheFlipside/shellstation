use keyring::Entry;

const SERVICE_NAME: &str = "shellstation";

/// Store a secret in the OS keychain.
pub fn store(keychain_ref: &str, secret: &str) -> Result<(), String> {
    let entry =
        Entry::new(SERVICE_NAME, keychain_ref).map_err(|e| format!("Keychain error: {e}"))?;
    entry
        .set_password(secret)
        .map_err(|e| format!("Failed to store credential: {e}"))
}

/// Retrieve a secret from the OS keychain.
pub fn retrieve(keychain_ref: &str) -> Result<String, String> {
    let entry =
        Entry::new(SERVICE_NAME, keychain_ref).map_err(|e| format!("Keychain error: {e}"))?;
    entry
        .get_password()
        .map_err(|e| format!("Failed to retrieve credential: {e}"))
}

/// Delete a secret from the OS keychain.
pub fn delete(keychain_ref: &str) -> Result<(), String> {
    let entry =
        Entry::new(SERVICE_NAME, keychain_ref).map_err(|e| format!("Keychain error: {e}"))?;
    entry
        .delete_credential()
        .map_err(|e| format!("Failed to delete credential: {e}"))
}
