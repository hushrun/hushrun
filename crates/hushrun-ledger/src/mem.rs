//! An in-memory ledger (`mem://name`), for tests. Ledgers with the same name share their
//! objects within a process.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::{Ledger, LedgerError, check_key};

type Objects = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;
type Registry = Mutex<HashMap<String, (Objects, Arc<AtomicBool>)>>;

#[derive(Debug, Clone)]
pub struct MemLedger {
    name: String,
    objects: Objects,
    failing: Arc<AtomicBool>,
}

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

impl MemLedger {
    /// The ledger called `name` in this process.
    pub fn named(name: &str) -> MemLedger {
        let mut registry = registry().lock().expect("mem ledger registry");
        let (objects, failing) = registry
            .entry(name.to_string())
            .or_insert_with(|| (Objects::default(), Arc::new(AtomicBool::new(false))))
            .clone();
        MemLedger {
            name: name.to_string(),
            objects,
            failing,
        }
    }

    /// A fresh, unnamed ledger.
    pub fn new() -> MemLedger {
        MemLedger {
            name: String::new(),
            objects: Objects::default(),
            failing: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Makes every request fail as `Unavailable` (to test L-5).
    pub fn set_unavailable(&self, unavailable: bool) {
        self.failing.store(unavailable, Ordering::SeqCst);
    }

    fn check(&self) -> Result<(), LedgerError> {
        if self.failing.load(Ordering::SeqCst) {
            Err(LedgerError::Unavailable(format!(
                "mem://{} is failing on purpose",
                self.name
            )))
        } else {
            Ok(())
        }
    }

    fn objects(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>> {
        self.objects.lock().expect("mem ledger objects")
    }
}

impl Default for MemLedger {
    fn default() -> Self {
        MemLedger::new()
    }
}

impl Ledger for MemLedger {
    fn append(&self, key: &str, bytes: &[u8]) -> Result<(), LedgerError> {
        self.check()?;
        check_key(key)?;
        let mut objects = self.objects();
        match objects.get(key) {
            Some(existing) if existing == bytes => Ok(()),
            Some(_) => Err(LedgerError::AlreadyExists(key.to_string())),
            None => {
                objects.insert(key.to_string(), bytes.to_vec());
                Ok(())
            }
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, LedgerError> {
        self.check()?;
        Ok(self
            .objects()
            .range(prefix.to_string()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, _)| k.clone())
            .collect())
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, LedgerError> {
        self.check()?;
        check_key(key)?;
        self.objects()
            .get(key)
            .cloned()
            .ok_or_else(|| LedgerError::NotFound(key.to_string()))
    }

    fn delete(&self, key: &str) -> Result<(), LedgerError> {
        self.check()?;
        self.objects()
            .remove(key)
            .map(|_| ())
            .ok_or_else(|| LedgerError::NotFound(key.to_string()))
    }
}
