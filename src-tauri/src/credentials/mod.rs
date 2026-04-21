use keyring::Entry;
use zeroize::Zeroizing;

const SERVICE_NAME: &str = "shellstation";

/// Store a secret in the OS keychain.
pub fn store(keychain_ref: &str, secret: &str) -> Result<(), String> {
    let entry = Entry::new(SERVICE_NAME, keychain_ref).map_err(|e| {
        tracing::error!(keychain_ref = %keychain_ref, error = %e, "Keychain entry creation failed");
        "Keychain error: failed to access credential store".to_string()
    })?;
    entry.set_password(secret).map_err(|e| {
        tracing::error!(keychain_ref = %keychain_ref, error = %e, "Keychain store failed");
        "Failed to store credential in keychain".to_string()
    })
}

/// Retrieve a secret from the OS keychain.
///
/// Returns `Zeroizing<String>` so the secret is wiped from memory on drop.
pub fn retrieve(keychain_ref: &str) -> Result<Zeroizing<String>, String> {
    let entry = Entry::new(SERVICE_NAME, keychain_ref).map_err(|e| {
        tracing::error!(keychain_ref = %keychain_ref, error = %e, "Keychain entry creation failed");
        "Keychain error: failed to access credential store".to_string()
    })?;
    entry.get_password().map(Zeroizing::new).map_err(|e| {
        tracing::error!(keychain_ref = %keychain_ref, error = %e, "Keychain retrieve failed");
        "Failed to retrieve credential from keychain".to_string()
    })
}

/// Delete a secret from the OS keychain.
pub fn delete(keychain_ref: &str) -> Result<(), String> {
    let entry = Entry::new(SERVICE_NAME, keychain_ref).map_err(|e| {
        tracing::error!(keychain_ref = %keychain_ref, error = %e, "Keychain entry creation failed");
        "Keychain error: failed to access credential store".to_string()
    })?;
    entry.delete_credential().map_err(|e| {
        tracing::error!(keychain_ref = %keychain_ref, error = %e, "Keychain delete failed");
        "Failed to delete credential from keychain".to_string()
    })
}
