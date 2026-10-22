//! Desktop host (feature `desktop-host`). Windows has no Kotlin or Swift layer,
//! so this module plays their role: it runs the application methods the
//! mobile hosts dispatch over `com.zarz.mornye/backend` and exposes them to
//! Dart through one C entry point. It mirrors `SelectedCoreBackend.kt` and
//! `RustCoreBackendApplication.kt`; method names, argument keys, replies and
//! error messages are the same contract.

use crate::manager::ExtensionManager;
use crate::repository::ExtensionRepository;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::ffi::{CStr, CString, c_char};
use std::fmt::Display;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Once};

type Args = Map<String, Value>;
type Reply = Result<Value, String>;

/// Version reported to extensions and HTTP peers. The Windows build passes the
/// pubspec version (without the build number), like Android's VERSION_NAME.
const APP_VERSION: &str = match option_env!("MORNYE_APP_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

struct Host {
    manager: Option<Arc<ExtensionManager>>,
    repository: Option<Arc<ExtensionRepository>>,
    /// Canonical source and data directories plus the master key digest.
    identity: Option<(String, String, Vec<u8>)>,
    logging_enabled: bool,
    allow_private_network: bool,
    allow_http_fallback: bool,
    fallback_providers: Option<Vec<String>>,
}

static HOST: Mutex<Host> = Mutex::new(Host {
    manager: None,
    repository: None,
    identity: None,
    logging_enabled: false,
    allow_private_network: false,
    allow_http_fallback: false,
    fallback_providers: None,
});
static WORKSPACE_RECLAIMED: Once = Once::new();

/// Runs `method` with JSON-encoded `arguments` and returns a JSON envelope,
/// `{"ok": <reply>}` or `{"error": "<message>"}`. Release it with
/// [`mornye_desktop_free`].
///
/// # Safety
/// Both pointers must be valid NUL-terminated strings.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mornye_desktop_invoke(
    method: *const c_char,
    arguments: *const c_char,
) -> *mut c_char {
    let reply = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the caller passes valid NUL-terminated strings.
        let method = unsafe { CStr::from_ptr(method) }
            .to_str()
            .map_err(|_| "Method name is not UTF-8".to_owned())?;
        // SAFETY: as above.
        let arguments = unsafe { CStr::from_ptr(arguments) }
            .to_str()
            .map_err(|_| "Arguments are not UTF-8".to_owned())?;
        let arguments = match serde_json::from_str(arguments).map_err(error)? {
            Value::Object(map) => map,
            _ => Args::new(),
        };
        invoke(method, &arguments)
    }))
    .unwrap_or_else(|_| Err("Rust backend panicked".to_owned()));
    let envelope = match reply {
        Ok(value) => json!({ "ok": value }),
        Err(message) => json!({ "error": message }),
    };
    // serde_json escapes NUL as \u0000, so the text never contains one.
    CString::new(envelope.to_string())
        .expect("JSON text has no NUL byte")
        .into_raw()
}

/// Frees a reply returned by [`mornye_desktop_invoke`].
///
/// # Safety
/// `reply` must come from `mornye_desktop_invoke` and be freed only once.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mornye_desktop_free(reply: *mut c_char) {
    if !reply.is_null() {
        // SAFETY: guaranteed by the caller contract above.
        drop(unsafe { CString::from_raw(reply) });
    }
}

fn invoke(method: &str, args: &Args) -> Reply {
    match method {
        "getBackendImplementations" => Ok(json!({
            "filename": "rust",
            "file_metadata": "rust",
            "extensions": "rust",
            "downloads": "rust",
        })),
        "initExtensionSystem" => initialize_owner(args),
        "cleanupExtensions" => {
            shutdown_owner();
            Ok(Value::Null)
        }
        "setDownloadDirectory" => set_download_directory(&string(args, "path", "")),
        "buildFilename" => text(crate::filename::build_filename(
            string(args, "template", ""),
            string(args, "metadata", "{}"),
        )),
        "sanitizeFilename" => Ok(Value::String(crate::sanitize_filename(string(
            args, "filename", "",
        )))),
        "getLogsSince" => {
            let Some(current) = host().manager.clone() else {
                return Ok(Value::String(r#"{"logs":[],"next_index":0}"#.into()));
            };
            let index = args.get("index").and_then(Value::as_i64).unwrap_or(0);
            text(current.environment().log_buffer().since(index))
        }
        "clearLogs" => {
            if let Some(current) = host().manager.clone() {
                current.environment().log_buffer().clear().map_err(error)?;
            }
            Ok(Value::Null)
        }
        "setLoggingEnabled"
        | "setAllowPrivateNetwork"
        | "setNetworkCompatibilityOptions"
        | "setDownloadFallbackExtensionIds"
        | "setLyricsProviders"
        | "setLyricsFetchOptions" => configure(method, args),
        "initExtensionRepo" => initialize_repository(&string(args, "cache_dir", "")),
        "setRepoRegistryUrl" => {
            unit(repository()?.set_registry_url(string(args, "registry_url", "")))
        }
        "getRepoRegistryUrl" => text(repository()?.registry_url()),
        "clearRepoRegistryUrl" => unit(repository()?.clear_registry_url()),
        "getRepoExtensions" => text(repository()?.extensions(flag(args, "force_refresh"))),
        "downloadRepoExtension" => text(repository()?.download(
            string(args, "extension_id", ""),
            string(args, "dest_dir", ""),
        )),
        _ => owner_method(method, args),
    }
}

/// Methods that require an initialized extension system.
fn owner_method(method: &str, args: &Args) -> Reply {
    let current = owner()?;
    match method {
        "loadExtensionsFromDir" => {
            let requested = canonical(&string(args, "dir_path", ""))?;
            if host().identity.as_ref().map(|identity| &identity.0) != Some(&requested) {
                return Err("Extension source directory does not match the initialized owner".into());
            }
            text(current.load_all())
        }
        "getInstalledExtensions" => text(current.installed()),
        "loadExtensionFromPath" => text(current.install(string(args, "file_path", ""))),
        "upgradeExtension" => text(current.upgrade(string(args, "file_path", ""))),
        "checkExtensionUpgrade" => text(current.check_upgrade(string(args, "file_path", ""))),
        "setExtensionEnabled" => unit(
            current.set_enabled(string(args, "extension_id", ""), flag(args, "enabled")),
        ),
        "removeExtension" => unit(current.remove(string(args, "extension_id", ""))),
        "getExtensionSettings" => text(
            current
                .environment()
                .settings(string(args, "extension_id", "")),
        ),
        "setExtensionSettings" => unit(current.update_settings(
            string(args, "extension_id", ""),
            string(args, "settings", "{}"),
        )),
        "invokeExtensionAction" => {
            let id = string(args, "extension_id", "");
            let action = string(args, "action", "");
            if args.contains_key("arguments_json") {
                // Transient form input bypasses persistent extension settings.
                let arguments = string(args, "arguments_json", "[]");
                text(current.call(id, action, arguments, None, 120_000))
            } else {
                text(current.invoke_action(id, action))
            }
        }
        "checkExtensionHealth" => {
            text(current.check_extension_health_json(string(args, "extension_id", "")))
        }
        "setProviderPriority" | "setMetadataProviderPriority" => {
            let priority = ids(&string(args, "priority", "[]"))?;
            unit(current.set_provider_priority(priority_kind(method).into(), priority))
        }
        "getProviderPriority" | "getMetadataProviderPriority" => {
            let priorities: Value =
                serde_json::from_str(&current.provider_priorities().map_err(error)?)
                    .map_err(error)?;
            let list = priorities
                .get(priority_kind(method))
                .filter(|value| value.is_array());
            Ok(Value::String(list.map_or_else(|| "[]".into(), Value::to_string)))
        }
        _ => Err(format!("Rust application method is not connected yet: {method}")),
    }
}

/// Settings the app may send before or after initialization; they are kept so
/// a later `initExtensionSystem` applies them, as on Android.
fn configure(method: &str, args: &Args) -> Reply {
    let mut host = host();
    let environment = host.manager.as_ref().map(|manager| manager.environment());
    match method {
        "setLoggingEnabled" => {
            let enabled = flag(args, "enabled");
            if let Some(environment) = &environment {
                environment.log_buffer().set_enabled(enabled).map_err(error)?;
            }
            host.logging_enabled = enabled;
        }
        "setAllowPrivateNetwork" => {
            let allowed = flag(args, "allowed");
            if let Some(environment) = &environment {
                environment.set_allow_private_network(allowed).map_err(error)?;
            }
            host.allow_private_network = allowed;
        }
        "setNetworkCompatibilityOptions" => {
            let allowed = flag(args, "allow_http");
            if let Some(environment) = &environment {
                environment
                    .set_network_compatibility_options(allowed, flag(args, "insecure_tls"))
                    .map_err(error)?;
            }
            host.allow_http_fallback = allowed;
        }
        "setDownloadFallbackExtensionIds" => {
            let raw = string(args, "extension_ids", "");
            let value = if raw.trim().is_empty() || raw.trim() == "null" {
                None
            } else {
                Some(ids(&raw)?)
            };
            if let Some(manager) = &host.manager {
                manager.set_fallback_providers(value.clone()).map_err(error)?;
            }
            host.fallback_providers = value;
        }
        "setLyricsProviders" => {
            let raw = string(args, "providers_json", "[]");
            match &host.manager {
                Some(manager) => manager.set_lyrics_providers_json(raw).map_err(error)?,
                None => drop(ids(&raw)?),
            }
            return Ok(Value::String(r#"{"success":true}"#.into()));
        }
        "setLyricsFetchOptions" => {
            let raw = string(args, "options_json", "{}");
            match &host.manager {
                Some(manager) => manager.set_lyrics_fetch_options_json(raw).map_err(error)?,
                None => drop(serde_json::from_str::<Map<String, Value>>(&raw).map_err(error)?),
            }
            return Ok(Value::String(r#"{"success":true}"#.into()));
        }
        _ => unreachable!("configure only receives configuration methods"),
    }
    Ok(Value::Null)
}

fn initialize_owner(args: &Args) -> Reply {
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

fn initialize_repository(cache_path: &str) -> Reply {
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

fn shutdown_owner() {
    let mut host = host();
    if let Some(repository) = host.repository.take() {
        repository.shutdown();
    }
    if let Some(manager) = host.manager.take() {
        manager.shutdown();
    }
    host.identity = None;
}

fn set_download_directory(path: &str) -> Reply {
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

fn host() -> MutexGuard<'static, Host> {
    HOST.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn owner() -> Result<Arc<ExtensionManager>, String> {
    host()
        .manager
        .clone()
        .ok_or_else(|| "Rust backend is not initialized".into())
}

fn repository() -> Result<Arc<ExtensionRepository>, String> {
    host()
        .repository
        .clone()
        .ok_or_else(|| "Extension repository is not initialized".into())
}

/// `setProviderPriority` and `getProviderPriority` address the download list.
fn priority_kind(method: &str) -> &'static str {
    if method.contains("Metadata") {
        "metadata"
    } else {
        "download"
    }
}

fn storage() -> Result<(String, String), String> {
    host()
        .identity
        .as_ref()
        .map(|(sources, data, _)| (sources.clone(), data.clone()))
        .ok_or_else(|| "Rust backend is not initialized".into())
}

/// Private scratch directory, the counterpart of Android's
/// `noBackupFilesDir/native-media-work`. Leftovers from a previous process
/// cannot resume, so they are removed once per process.
fn workspace() -> Result<String, String> {
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

/// Canonical and absolute spellings of an output directory, rejecting a
/// filesystem root and any overlap with extension storage.
fn directory_aliases(value: &str, sources: &str, data: &str) -> Result<Vec<String>, String> {
    if !Path::new(value).is_absolute() {
        return Err("Output directories must be absolute".into());
    }
    let path = canonical(value)?;
    let overlaps = |other: &str| within(&path, other) || within(other, &path);
    if Path::new(&path).parent().is_none() || overlaps(sources) || overlaps(data) {
        return Err("Output directory overlaps extension storage".into());
    }
    let absolute = plain(std::path::absolute(value).map_err(error)?);
    let mut aliases = vec![path];
    push_unique(&mut aliases, vec![absolute]);
    Ok(aliases)
}

/// Like Java's `File.canonicalPath`: resolves existing paths and falls back to
/// the normalized absolute path for ones that do not exist yet.
fn canonical(path: &str) -> Result<String, String> {
    let path = Path::new(path);
    let resolved = match std::fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(_) => std::path::absolute(path).map_err(error)?,
    };
    Ok(plain(resolved))
}

/// Drops the `\\?\` prefix `canonicalize` adds to drive paths, so the result
/// matches the paths Dart sends and compares against.
fn plain(path: PathBuf) -> String {
    let text = path.to_string_lossy().into_owned();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_owned(),
        _ => text,
    }
}

/// Whether `path` equals or is inside `parent`. Windows paths are
/// case-insensitive.
fn within(path: &str, parent: &str) -> bool {
    Path::new(&path.to_lowercase()).starts_with(parent.to_lowercase())
}

fn push_unique(target: &mut Vec<String>, values: Vec<String>) {
    for value in values {
        if !target.contains(&value) {
            target.push(value);
        }
    }
}

fn required<'a>(args: &'a Args, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing {key}"))
}

fn string(args: &Args, key: &str, default: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

fn flag(args: &Args, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn ids(raw: &str) -> Result<Vec<String>, String> {
    serde_json::from_str(raw).map_err(error)
}

fn text<E: Display>(result: Result<String, E>) -> Reply {
    result.map(Value::String).map_err(error)
}

fn unit<E: Display>(result: Result<(), E>) -> Reply {
    result.map(|()| Value::Null).map_err(error)
}

fn error(error: impl Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_methods_require_initialization() {
        for method in ["getInstalledExtensions", "noSuchMethod"] {
            let reply = invoke(method, &Args::new());
            assert_eq!(reply, Err("Rust backend is not initialized".into()), "{method}");
        }
    }

    #[test]
    fn plain_strips_only_drive_verbatim_prefixes() {
        assert_eq!(plain(PathBuf::from(r"\\?\C:\Music")), r"C:\Music");
        assert_eq!(
            plain(PathBuf::from(r"\\?\UNC\server\share")),
            r"\\?\UNC\server\share"
        );
    }

    #[test]
    fn output_directories_cannot_overlap_storage_or_be_a_root() {
        let storage = std::env::temp_dir().join("mornye-desktop-test");
        let sources = storage.join("sources");
        let data = storage.join("data");
        let (sources, data) = (sources.to_string_lossy(), data.to_string_lossy());
        for value in [
            storage.to_string_lossy().into_owned(),
            format!("{data}\\inner"),
            sources.to_uppercase(),
            r"C:\".to_owned(),
            "relative".to_owned(),
        ] {
            assert!(directory_aliases(&value, &sources, &data).is_err(), "{value}");
        }
        let music = storage.join("Music");
        assert!(directory_aliases(&music.to_string_lossy(), &sources, &data).is_ok());
    }

    #[test]
    fn envelopes_wrap_replies_and_errors() {
        let method = CString::new("sanitizeFilename").unwrap();
        let arguments = CString::new(r#"{"filename":"a/b"}"#).unwrap();
        // SAFETY: valid strings; the reply is freed once below.
        #[allow(unsafe_code)]
        let reply = unsafe {
            let raw = mornye_desktop_invoke(method.as_ptr(), arguments.as_ptr());
            let text = CStr::from_ptr(raw).to_str().unwrap().to_owned();
            mornye_desktop_free(raw);
            text
        };
        let envelope: Value = serde_json::from_str(&reply).unwrap();
        assert!(envelope["ok"].is_string(), "{reply}");
    }
}
