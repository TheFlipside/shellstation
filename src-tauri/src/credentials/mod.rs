use keyring::Entry;
use zeroize::Zeroizing;

const SERVICE_NAME: &str = "shellstation";

/// Store a secret in the `OS` keychain.
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

/// Retrieve a secret from the `OS` keychain.
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

/// Delete a secret from the `OS` keychain if there is one. An entry that is
/// not there is no failure, and is not logged as one: nothing is left of it.
pub fn delete_if_present(keychain_ref: &str) -> Result<(), String> {
    let entry = Entry::new(SERVICE_NAME, keychain_ref).map_err(|e| {
        tracing::error!(keychain_ref = %keychain_ref, error = %e, "Keychain entry creation failed");
        "Keychain error: failed to access credential store".to_string()
    })?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => {
            tracing::error!(keychain_ref = %keychain_ref, error = %e, "Keychain delete failed");
            Err("Failed to delete credential from keychain".to_string())
        }
    }
}

/// Delete a secret from the `OS` keychain.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes to the real `OS` keychain and deletes from it, so it runs only
    /// when asked for: `cargo test --lib credentials::tests -- --ignored`.
    #[test]
    #[ignore = "writes to and deletes from the OS keychain"]
    fn deleting_if_present_takes_an_entry_away_and_is_no_failure_without_one() {
        /// Takes the entry away again, also when the test fails part way.
        struct Throwaway(String);
        impl Drop for Throwaway {
            fn drop(&mut self) {
                let _ = delete_if_present(&self.0);
            }
        }

        let throwaway = Throwaway(format!("test-delete-if-present-{}", uuid::Uuid::new_v4()));
        let keychain_ref = throwaway.0.clone();
        store(&keychain_ref, "throwaway").expect("store an entry");
        assert_eq!(
            retrieve(&keychain_ref).expect("read it back").as_str(),
            "throwaway"
        );

        delete_if_present(&keychain_ref).expect("delete an entry that is there");
        assert!(retrieve(&keychain_ref).is_err(), "the entry is still there");
        delete_if_present(&keychain_ref).expect("delete an entry that is not there");
    }
}
