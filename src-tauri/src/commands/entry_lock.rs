//! One change of a stored entry at a time.
//!
//! A change of a credential profile is two steps, its row and its secret in
//! the `OS` keychain, and the commands that make one run side by side. Two
//! changes of one profile could otherwise take their steps in turns and
//! leave the row of the one with the secret of the other, and the manager's
//! read of the secret could come between a change's two steps. Whoever
//! changes or deletes an entry, or reads its secret for the manager, holds
//! the entry's lock for as long as it takes, so each of them finds the entry
//! as the one before left it.
//!
//! The window keeps a second change of an entry from being sent while one is
//! under way (`updating` in the stores). That is forgotten when its page is
//! loaded again, while a command the old page sent may still be running:
//! this is what holds then.
//!
//! An entry waits for itself alone. A keychain that does not answer, as one
//! whose prompt nobody answers, holds up the entry it was asked about and no
//! other.
//!
//! It is for this process alone. A second instance of the app has locks of
//! its own. Nothing here says in which order two changes arrive: only that
//! they do not overlap. And a connection that reads a profile's secret does
//! not wait here.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex as StdMutex, PoisonError, Weak};

use tokio::sync::{Mutex, OwnedMutexGuard};
use uuid::Uuid;

/// The lock of each entry that is held or waited for. An entry that nobody
/// holds or waits for has none: its lock is gone with the last of them, and
/// what is left of it here is taken out by whoever comes next.
static ENTRY_LOCKS: LazyLock<StdMutex<HashMap<Uuid, Weak<Mutex<()>>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

/// The lock of an entry, made if nobody has it.
fn lock_of(id: Uuid) -> Arc<Mutex<()>> {
    // Nothing is done under this lock that could fail part way, so what a
    // holder that panicked left behind is as good as any.
    let mut locks = ENTRY_LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(&id).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(id, Arc::downgrade(&lock));
    lock
}

/// Wait for the entry, and hold it until the guard is dropped.
pub async fn hold(id: Uuid) -> OwnedMutexGuard<()> {
    lock_of(id).lock_owned().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Whether an entry has a lock that is held or waited for.
    fn has_lock(id: Uuid) -> bool {
        let locks = ENTRY_LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
        locks.get(&id).is_some_and(|lock| lock.strong_count() > 0)
    }

    #[tokio::test]
    async fn a_second_change_of_an_entry_waits_for_the_first() {
        let id = Uuid::new_v4();
        let first = hold(id).await;

        let mut second = Box::pin(hold(id));
        let waited = tokio::time::timeout(Duration::from_millis(50), &mut second).await;
        assert!(waited.is_err(), "the entry was taken while it was held");

        drop(first);
        let _second = tokio::time::timeout(Duration::from_secs(5), second)
            .await
            .expect("the entry was not given up");
    }

    #[tokio::test]
    async fn an_entry_does_not_wait_for_another() {
        let _held = hold(Uuid::new_v4()).await;
        let _other = tokio::time::timeout(Duration::from_secs(5), hold(Uuid::new_v4()))
            .await
            .expect("an entry waited for another one");
    }

    #[tokio::test]
    async fn an_entry_that_is_given_up_leaves_no_lock_behind() {
        let id = Uuid::new_v4();
        let first = hold(id).await;
        let mut second = Box::pin(hold(id));
        let waited = tokio::time::timeout(Duration::from_millis(50), &mut second).await;
        assert!(waited.is_err(), "the entry was taken while it was held");

        // The one that waits keeps the lock by itself, once the one that held
        // it has gone and before it has the entry.
        drop(first);
        assert!(has_lock(id));
        let second = second.await;
        assert!(has_lock(id));
        drop(second);
        assert!(!has_lock(id));

        // What is left of it is taken out by the next one that comes.
        let _other = hold(Uuid::new_v4()).await;
        let locks = ENTRY_LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(!locks.contains_key(&id));
    }
}
