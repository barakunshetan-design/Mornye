//! Desktop host (feature `desktop-host`). Windows has no Kotlin or Swift layer,
//! so this module plays their role: it runs the application methods the
//! mobile hosts dispatch over `com.zarz.mornye/backend` and exposes them to
//! Dart through one C entry point. It mirrors `SelectedCoreBackend.kt` and
//! `RustCoreBackendApplication.kt`; method names, argument keys, replies and
//! error messages are the same contract.

mod args;
mod ffmpeg;
mod host;
mod methods;
mod paths;
#[cfg(test)]
mod tests;

use args::{error, flag, string, text, unit};
use ffmpeg::with_ffmpeg;
use host::{
    acquire_download_directory, host, initialize_owner, initialize_repository, owner, repository,
    request_registry, set_download_directory, shutdown_owner,
};
use serde_json::{Map, Value, json};
use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

type Args = Map<String, Value>;
type Reply = Result<Value, String>;

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
        let arguments: Value = serde_json::from_str(arguments).map_err(error)?;
        // These two take the raw request JSON string instead of a map.
        if method == "findCollectionAcrossExtensions" {
            let request = arguments.as_str().unwrap_or("{}").to_owned();
            return text(owner()?.find_collection_across_extensions_json(request, None));
        }
        if method == "downloadByStrategy" {
            let request = arguments.as_str().unwrap_or("{}").to_owned();
            let current = owner()?;
            return with_ffmpeg(&current, || text(current.download_by_strategy(request)));
        }
        let arguments = match arguments {
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

/// Methods that work before the extension system starts, or manage it.
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
        "acquireDownloadDirectory" => acquire_download_directory(&string(args, "path", "")),
        "releaseDownloadDirectory" => {
            let scope = host().directory_scopes.remove(&string(args, "token", ""));
            if let Some(scope) = scope {
                scope.release();
            }
            Ok(Value::Null)
        }
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
        | "setLyricsFetchOptions" => methods::configure(method, args),
        "cancelExtensionRequest" => unit(request_registry().cancel(string(args, "request_id", ""))),
        "customSearchWithExtension" | "getExtensionHomeFeed" => {
            let current = owner()?;
            let lease = request_registry()
                .acquire(string(args, "request_id", ""))
                .map_err(error)?;
            let id = string(args, "extension_id", "");
            let reply = if method == "customSearchWithExtension" {
                current.custom_search_json(
                    id,
                    string(args, "query", ""),
                    string(args, "options", ""),
                    Some(Arc::clone(&lease)),
                )
            } else {
                current.get_extension_home_feed_json(id, Some(Arc::clone(&lease)))
            };
            lease.release();
            text(reply)
        }
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
        _ => methods::owner_method(method, args),
    }
}
