//! Process-wide host state: the extension owner, its repository, request and
//! directory grants, and settings applied when the owner starts.

use super::args::{error, required, string};
use super::paths::{canonical, directory_aliases, push_unique};
use super::{Args, Reply};
use crate::cancellation::{CancellationDomain, CancellationRegistry};
use crate::extensions::DownloadDirectoryScope;
use crate::manager::ExtensionManager;
use crate::repository::ExtensionRepository;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once};
use std::time::{SystemTime, UNIX_EPOCH};

/// Version reported to extensions and HTTP peers. The Windows build passes the
/// pubspec version (without the build number), like Android's VERSION_NAME.
const APP_VERSION: &str = match option_env!("MORNYE_APP_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

pub(super) struct Host {
    pub(super) manager: Option<Arc<ExtensionManager>>,
    repository: Option<Arc<ExtensionRepository>>,
    /// Cancellable extension requests (searches, home feeds), by request ID.
    requests: Option<Arc<CancellationRegistry>>,
    /// Temporary output directory grants, by the token handed to Dart.
    pub(super) directory_scopes: BTreeMap<String, Arc<DownloadDirectoryScope>>,
    /// Canonical source and data directories plus the master key digest.
    pub(super) identity: Option<(String, String, Vec<u8>)>,
    pub(super) logging_enabled: bool,
    pub(super) allow_private_network: bool,
    pub(super) allow_http_fallback: bool,
    pub(super) fallback_providers: Option<Vec<String>>,
}

static HOST: Mutex<Host> = Mutex::new(Host {
    manager: None,
    repository: None,
    requests: None,
    directory_scopes: BTreeMap::new(),
    identity: None,
    logging_enabled: false,
    allow_private_network: false,
    allow_http_fallback: false,
    fallback_providers: None,
});
static WORKSPACE_RECLAIMED: Once = Once::new();

pub(super) fn initialize_owner(args: &Args) -> Reply {
    let source_path = required(args, "extensions_dir")?;
    let data_path = required(args, "data_dir")?;
    if !Path::new(source_path).is_absolute() || !Path::new(data_path).is_absolute() {
        return Err("Extension storage directories must be absolute".into());
    }
    let sources = canonical(source_path)?;
    let data = canonical(data_path)?;
    let key = required(args, "master_key")?;
    let requested = (
        sources.clone(),
        data.clone(),
        Sha256::digest(key.as_bytes()).to_vec(),
    );
    let mut allowed = vec![workspace()?];
    match args.get("allowed_directories") {
        None | Some(Value::Null) => {}
        Some(Value::Array(values)) => {
            for value in values {
                let value = value.as_str().ok_or("Output directories must be strings")?;
                push_unique(&mut allowed, directory_aliases(value, &sources, &data)?);
            }
        }
        Some(_) => return Err("Invalid output directories".into()),
    }

    let mut host = host();
    if let Some(manager) = &host.manager {
        if host.identity.as_ref() != Some(&requested) {
            return Err("Rust backend is already initialized with different storage".into());
        }
        manager
            .environment()
            .set_allowed_download_directories(allowed)
            .map_err(error)?;
        return Ok(Value::Null);
    }
    let created = ExtensionManager::with_lyrics_settings(
        sources,
        data,
        key.to_owned(),
        APP_VERSION.to_owned(),
        30_000,
        string(args, "lyrics_providers_json", "[]"),
        string(args, "lyrics_options_json", "{}"),
    )
    .map_err(error)?;
    let configured = (|| {
        let environment = created.environment();
        environment
            .set_allowed_download_directories(allowed)
            .map_err(error)?;
        environment
            .set_allow_private_network(host.allow_private_network)
            .map_err(error)?;
        environment
            .set_network_compatibility_options(host.allow_http_fallback, false)
            .map_err(error)?;
        environment
            .log_buffer()
            .set_enabled(host.logging_enabled)
            .map_err(error)?;
        created
            .set_fallback_providers(host.fallback_providers.clone())
            .map_err(error)
    })();
    if let Err(message) = configured {
        created.shutdown();
        return Err(message);
    }
    host.identity = Some(requested);
    host.manager = Some(Arc::new(created));
    Ok(Value::Null)
}

pub(super) fn initialize_repository(cache_path: &str) -> Reply {
    let current = owner()?;
    let mut host = host();
    if host.repository.is_some() {
        return Ok(Value::Null);
    }
    if !Path::new(cache_path).is_absolute() {
        return Err("Repository cache directory must be absolute".into());
    }
    let repository = ExtensionRepository::new(current, canonical(cache_path)?).map_err(error)?;
    host.repository = Some(Arc::new(repository));
    Ok(Value::Null)
}

pub(super) fn acquire_download_directory(path: &str) -> Reply {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let current = owner()?;
    let (sources, data) = storage()?;
    let scope = current
        .environment()
        .grant_download_directories(directory_aliases(path, &sources, &data)?)
        .map_err(error)?;
    // Unguessable is not required: the token only names a grant Dart created.
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let token = format!("{stamp:x}-{:x}", SERIAL.fetch_add(1, Ordering::Relaxed));
    host().directory_scopes.insert(token.clone(), scope);
    Ok(Value::String(token))
}

pub(super) fn shutdown_owner() {
    let mut host = host();
    for scope in std::mem::take(&mut host.directory_scopes).into_values() {
        scope.release();
    }
    if let Some(requests) = host.requests.take() {
        requests.shutdown();
    }
    if let Some(repository) = host.repository.take() {
        repository.shutdown();
    }
    if let Some(manager) = host.manager.take() {
        manager.shutdown();
    }
    host.identity = None;
}

pub(super) fn set_download_directory(path: &str) -> Reply {
    let current = owner()?;
    let (sources, data) = storage()?;
    let mut allowed = vec![workspace()?];
    if !path.is_empty() {
        push_unique(&mut allowed, directory_aliases(path, &sources, &data)?);
    }
    current
        .environment()
        .set_allowed_download_directories(allowed)
        .map_err(error)?;
    Ok(Value::Null)
}

pub(super) fn host() -> MutexGuard<'static, Host> {
    HOST.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) fn owner() -> Result<Arc<ExtensionManager>, String> {
    host()
        .manager
        .clone()
        .ok_or_else(|| "Rust backend is not initialized".into())
}

pub(super) fn request_registry() -> Arc<CancellationRegistry> {
    let mut host = host();
    let registry = host.requests.get_or_insert_with(|| {
        Arc::new(CancellationRegistry::new(
            CancellationDomain::ExtensionRequest,
        ))
    });
    Arc::clone(registry)
}

pub(super) fn repository() -> Result<Arc<ExtensionRepository>, String> {
    host()
        .repository
        .clone()
        .ok_or_else(|| "Extension repository is not initialized".into())
}

pub(super) fn storage() -> Result<(String, String), String> {
    host()
        .identity
        .as_ref()
        .map(|(sources, data, _)| (sources.clone(), data.clone()))
        .ok_or_else(|| "Rust backend is not initialized".into())
}

/// Private scratch directory, the counterpart of Android's
/// `noBackupFilesDir/native-media-work`. Leftovers from a previous process
/// cannot resume, so they are removed once per process.
pub(super) fn workspace() -> Result<String, String> {
    let base = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
    let directory = PathBuf::from(base)
        .join("com.zarz")
        .join("mornye")
        .join("native-media-work");
    std::fs::create_dir_all(&directory)
        .map_err(|e| format!("Could not prepare native media workspace: {e}"))?;
    WORKSPACE_RECLAIMED.call_once(|| {
        if let Ok(entries) = std::fs::read_dir(&directory) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    });
    canonical(&directory.to_string_lossy())
}
