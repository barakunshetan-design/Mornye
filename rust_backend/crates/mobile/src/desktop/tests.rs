use super::args::require_successful_action;
use super::ffmpeg::{FfmpegCommand, ffmpeg_commands, run_ffmpeg_command};
use super::paths::{directory_aliases, plain};
use super::*;
use std::path::PathBuf;

#[test]
fn owner_methods_require_initialization() {
    for method in ["getInstalledExtensions", "noSuchMethod"] {
        let reply = invoke(method, &Args::new());
        assert_eq!(
            reply,
            Err("Rust backend is not initialized".into()),
            "{method}"
        );
    }
}

#[test]
fn extension_actions_must_report_success() {
    assert!(require_successful_action("x", "completeGrant", r#"{"success":true}"#).is_ok());
    assert_eq!(
        require_successful_action(
            "x",
            "completeGrant",
            r#"{"success":false,"error":" ","message":"denied"}"#
        ),
        Err("Extension completeGrant failed for x: denied".into())
    );
    assert_eq!(
        require_successful_action("x", "completeGrant", "oops"),
        Err("Extension completeGrant for x returned invalid JSON: oops".into())
    );
}

#[test]
fn ffmpeg_commands_skip_entries_without_an_id() {
    let commands = ffmpeg_commands(
        r#"[{"command_id":"a","arguments":["-i","in","-y","out.flac"],"output_path":"out.flac"},
            {"command_id":" ","arguments":["-i"]}, 7]"#,
    );
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].id, "a");
    assert_eq!(
        commands[0].arguments.last().map(String::as_str),
        Some("out.flac")
    );
    assert_eq!(commands[0].output_path, "out.flac");
}

#[test]
fn ffmpeg_publishes_only_successful_output() {
    let directory = std::env::temp_dir().join(format!("mornye-ffmpeg-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let target = directory.join("out.txt");
    let command = |arguments: Vec<String>| FfmpegCommand {
        id: "a".into(),
        output_path: target.to_string_lossy().into_owned(),
        arguments,
    };
    let path = target.to_string_lossy().into_owned();
    let mismatched = command(vec!["-i".into(), "in".into()]);
    assert!(run_ffmpeg_command(&mismatched, &|| true).is_err());
    // An unknown FFmpeg option fails: nothing may appear under the target name.
    let failing = command(vec!["-no-such-option".into(), path]);
    if let Ok((success, _)) = run_ffmpeg_command(&failing, &|| true) {
        assert!(!success);
    }
    assert!(!target.exists());
    let leftovers = std::fs::read_dir(&directory).unwrap().count();
    assert_eq!(leftovers, 0, "staged output must be removed");
    std::fs::remove_dir_all(&directory).unwrap();
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
    let inner = data.join("inner");
    let (sources, data) = (sources.to_string_lossy(), data.to_string_lossy());
    let mut rejected = vec![
        storage.to_string_lossy().into_owned(),
        inner.to_string_lossy().into_owned(),
        "relative".to_owned(),
    ];
    // Windows paths are case-insensitive and rooted at a drive.
    if cfg!(windows) {
        rejected.extend([sources.to_uppercase(), r"C:\".to_owned()]);
    } else {
        rejected.push("/".to_owned());
    }
    for value in rejected {
        assert!(
            directory_aliases(&value, &sources, &data).is_err(),
            "{value}"
        );
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
