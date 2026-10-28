//! Process runtime environment.
//!
//! The process snapshots the OS environment once at startup and then scrubs the
//! secret copies from it. `setenv`/`unsetenv` concurrent with `getenv`,
//! `environ` iteration and fork/exec is not thread-safe on glibc, and a hang
//! there is invisible to test/step timeouts (docs/07 "CI 测试卡死"). Runtime
//! configuration is startup-only: saving settings changes neither this store
//! nor the OS environment. Startup secret scrubbing and tests use the guarded
//! overrides without mutating the process environment concurrently.

use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

/// `None` records an explicit clear (the OS value, if any, stops applying).
type Store = BTreeMap<String, Option<String>>;

fn store() -> &'static RwLock<Store> {
    static STORE: OnceLock<RwLock<Store>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(BTreeMap::new()))
}

fn read() -> std::sync::RwLockReadGuard<'static, Store> {
    store()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write() -> std::sync::RwLockWriteGuard<'static, Store> {
    store()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Snapshot the whole OS environment. Idempotent; call once at process start,
/// before any thread spawns a child process.
pub fn init_from_process_env() {
    let mut guard = write();
    for (key, value) in std::env::vars() {
        guard.entry(key).or_insert(Some(value));
    }
}

/// Runtime value: the store wins (it also records explicit clears); a key the
/// store never saw falls back to the OS environment (unit tests).
pub fn var(key: &str) -> Option<String> {
    match read().get(key) {
        Some(value) => value.clone(),
        None => std::env::var(key).ok(),
    }
}

/// Effective environment map: the OS environment overlaid with the store.
pub fn vars() -> BTreeMap<String, String> {
    let mut vars: BTreeMap<String, String> = std::env::vars().collect();
    for (key, value) in read().iter() {
        match value {
            Some(value) => {
                vars.insert(key.clone(), value.clone());
            }
            None => {
                vars.remove(key);
            }
        }
    }
    vars
}

/// Startup/test override; never call this when saving configuration.
/// `None` records an explicit clear.
#[cfg(test)]
pub fn set(key: &str, value: Option<&str>) {
    write().insert(key.to_string(), value.map(str::to_string));
}

/// Drop the override so the OS environment value (if any) applies again.
#[cfg(test)]
pub fn reset(key: &str) {
    write().remove(key);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_overrides_and_clears_without_touching_the_os_env() {
        let key = "NUWA_TEST_ENV_STORE_KEY";
        reset(key);
        assert_eq!(var(key), None, "an unknown key has no value");

        set(key, Some("v1"));
        assert_eq!(var(key).as_deref(), Some("v1"));
        assert_eq!(vars().get(key).map(String::as_str), Some("v1"));

        set(key, Some("v2"));
        assert_eq!(var(key).as_deref(), Some("v2"));

        set(key, None);
        assert_eq!(var(key), None, "an explicit clear beats the OS env");
        assert!(!vars().contains_key(key));

        reset(key);
        assert_eq!(var(key), None);
        assert!(std::env::var(key).is_err(), "the OS env was never touched");
    }
}
