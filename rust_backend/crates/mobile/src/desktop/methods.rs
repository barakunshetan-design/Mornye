//! Application methods that need an initialized extension system, and the
//! settings that are kept until one starts.

use super::args::{error, flag, ids, priority_kind, require_successful_action, string, text, unit};
use super::ffmpeg::with_ffmpeg;
use super::host::{host, owner, workspace};
use super::paths::{canonical, with_media_files};
use super::{Args, Reply};
use serde_json::{Map, Value, json};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn owner_method(method: &str, args: &Args) -> Reply {
    let current = owner()?;
    match method {
        "loadExtensionsFromDir" => {
            let requested = canonical(&string(args, "dir_path", ""))?;
            if host().identity.as_ref().map(|identity| &identity.0) != Some(&requested) {
                return Err(
                    "Extension source directory does not match the initialized owner".into(),
                );
            }
            text(current.load_all())
        }
        "getInstalledExtensions" => text(current.installed()),
        "loadExtensionFromPath" => text(current.install(string(args, "file_path", ""))),
        "upgradeExtension" => text(current.upgrade(string(args, "file_path", ""))),
        "checkExtensionUpgrade" => text(current.check_upgrade(string(args, "file_path", ""))),
        "setExtensionEnabled" => {
            unit(current.set_enabled(string(args, "extension_id", ""), flag(args, "enabled")))
        }
        "removeExtension" => unit(current.remove(string(args, "extension_id", ""))),
        "getExtensionSettings" => text(current.environment().settings(string(
            args,
            "extension_id",
            "",
        ))),
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
            Ok(Value::String(
                list.map_or_else(|| "[]".into(), Value::to_string),
            ))
        }
        "searchTracksWithMetadataProviders" => text(
            current.search_metadata_providers(
                string(args, "query", ""),
                args.get("limit").and_then(Value::as_i64).unwrap_or(20),
                args.get("include_extensions")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                String::new(),
                30_000,
            ),
        ),
        "searchTracksWithMetadataProvider" => text(current.search_metadata_provider(
            string(args, "extension_id", ""),
            string(args, "query", ""),
            args.get("limit").and_then(Value::as_i64).unwrap_or(20),
            30_000,
        )),
        "getProviderMetadata" => text(current.get_provider_metadata_json(
            string(args, "provider_id", ""),
            string(args, "resource_type", ""),
            string(args, "resource_id", ""),
            None,
        )),
        "searchDeezerByISRC" => text(current.search_deezer_by_isrc_for_item_id(
            string(args, "isrc", ""),
            string(args, "item_id", ""),
            None,
        )),
        "getDeezerExtendedMetadata" => {
            text(current.get_deezer_extended_metadata(string(args, "track_id", ""), None))
        }
        "convertSpotifyToDeezer" => text(current.convert_spotify_to_deezer(
            string(args, "resource_type", ""),
            string(args, "spotify_id", ""),
            None,
        )),
        "getTrackPlatformLinks" => text(current.get_track_platform_links_json(
            string(args, "spotify_id", ""),
            string(args, "isrc", ""),
            None,
        )),
        "fetchMusicBrainzTags" => {
            // Each tag is optional; a lookup failure leaves it empty.
            let isrc = string(args, "isrc", "");
            let genre = current
                .fetch_music_brainz_genre_by_isrc(isrc.clone(), None)
                .unwrap_or_default();
            let album_artist = current
                .fetch_music_brainz_album_artist_by_isrc(isrc, string(args, "album_name", ""), None)
                .unwrap_or_default();
            Ok(Value::String(
                json!({ "genre": genre, "album_artist": album_artist }).to_string(),
            ))
        }
        "getTrackCacheSize" => current
            .get_track_cache_size()
            .map(Value::from)
            .map_err(error),
        "clearTrackCache" => unit(current.clear_track_id_cache()),
        "setMetadataLanguage" => unit(current.set_metadata_language(string(args, "tag", ""))),
        "findURLHandler" => current
            .find_url_handler(string(args, "url", ""))
            .map(|handler| Value::String(handler.unwrap_or_default()))
            .map_err(error),
        "handleURLWithExtension" => text(current.handle_url_json(string(args, "url", ""))),
        "runPostProcessingV2" => {
            let raw = string(args, "input", "");
            let mut input = if raw.trim().is_empty() {
                Map::new()
            } else {
                serde_json::from_str(&raw).map_err(error)?
            };
            let path = input
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if !path.is_empty() {
                let name = Path::new(&path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                input.insert("name".into(), Value::String(name));
                input.insert("is_saf".into(), Value::Bool(false));
            }
            let input = Value::Object(input).to_string();
            let metadata = string(args, "metadata", "");
            with_ffmpeg(&current, || {
                text(current.run_post_processing(input, metadata, 120_000))
            })
        }
        // Tag writers reply with {"error": ...} instead of failing, like Android.
        "editFileMetadata" | "writeM4AFreeformTags" => {
            let path = string(args, "file_path", "");
            let metadata = string(args, "metadata_json", "{}");
            let cover = serde_json::from_str::<Value>(&metadata)
                .ok()
                .and_then(|value| value.get("cover_path")?.as_str().map(str::to_owned))
                .unwrap_or_default();
            let reply = with_media_files(&current, &[&path, &cover], || {
                let target = canonical(&path)?;
                if method == "editFileMetadata" {
                    current.edit_file_metadata(target, metadata, None)
                } else {
                    current.write_m4a_freeform_tags(target, metadata, None)
                }
                .map_err(error)
            });
            Ok(Value::String(reply.unwrap_or_else(|message| {
                json!({ "error": message }).to_string()
            })))
        }
        "downloadCoverToFile" => {
            let output = string(args, "output_path", "");
            let reply = (|| {
                let destination = if output.trim().is_empty() {
                    let stamp = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos();
                    Path::new(&workspace()?)
                        .join(format!("cover_{stamp}.jpg"))
                        .to_string_lossy()
                        .into_owned()
                } else {
                    canonical(&output)?
                };
                let max_dimension = args
                    .get("max_dimension")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    .max(0);
                current
                    .download_cover_to_file_sized(
                        string(args, "cover_url", ""),
                        destination.clone(),
                        max_dimension,
                        None,
                    )
                    .map_err(error)?;
                Ok::<_, String>(destination)
            })();
            Ok(Value::String(match reply {
                Ok(path) => json!({ "success": true, "file_path": path }).to_string(),
                Err(message) => json!({ "success": false, "error": message }).to_string(),
            }))
        }
        "getAllDownloadProgress" => text(current.environment().download_state().all_progress()),
        "cleanupConnections" => unit(current.environment().cleanup_connections()),
        "resetDownloadCancels" | "cancelDownloads" => {
            let item_ids = args
                .get("item_ids")
                .and_then(Value::as_array)
                .ok_or("Expected item_ids list")?
                .iter()
                .map(|id| {
                    id.as_str()
                        .map(str::to_owned)
                        .ok_or("Expected string item ID")
                })
                .collect::<Result<Vec<_>, _>>()?;
            let state = current.environment().download_state();
            if method == "cancelDownloads" {
                unit(state.cancel_downloads(item_ids))
            } else {
                unit(state.reset_download_cancels(item_ids))
            }
        }
        "clearItemProgress" | "cancelDownload" | "resetDownloadCancel" => {
            let item_id = string(args, "item_id", "");
            let state = current.environment().download_state();
            match method {
                "clearItemProgress" => unit(state.clear_item_progress(item_id)),
                "cancelDownload" => unit(state.cancel_download(item_id)),
                _ => unit(state.reset_download_cancel(item_id)),
            }
        }
        "completeExtensionSessionGrant" => {
            let id = string(args, "extension_id", "");
            current
                .environment()
                .set_session_grant(id.clone(), string(args, "grant", ""))
                .map_err(error)?;
            let response = current
                .invoke_action(id.clone(), "completeGrant".into())
                .map_err(error)?;
            require_successful_action(&id, "completeGrant", &response)?;
            Ok(Value::Bool(true))
        }
        "getExtensionPendingAuth" => current
            .get_extension_pending_auth_json(string(args, "extension_id", ""))
            .map(|pending| {
                if pending.is_empty() {
                    Value::Null
                } else {
                    Value::String(pending)
                }
            })
            .map_err(error),
        _ => Err(format!(
            "Rust application method is not connected yet: {method}"
        )),
    }
}

/// Settings the app may send before or after initialization; they are kept so
/// a later `initExtensionSystem` applies them, as on Android.
pub(super) fn configure(method: &str, args: &Args) -> Reply {
    let mut host = host();
    let environment = host.manager.as_ref().map(|manager| manager.environment());
    match method {
        "setLoggingEnabled" => {
            let enabled = flag(args, "enabled");
            if let Some(environment) = &environment {
                environment
                    .log_buffer()
                    .set_enabled(enabled)
                    .map_err(error)?;
            }
            host.logging_enabled = enabled;
        }
        "setAllowPrivateNetwork" => {
            let allowed = flag(args, "allowed");
            if let Some(environment) = &environment {
                environment
                    .set_allow_private_network(allowed)
                    .map_err(error)?;
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
                manager
                    .set_fallback_providers(value.clone())
                    .map_err(error)?;
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
