//! Runs the FFmpeg commands the backend queues, the job Android's
//! `withCoreFFmpegExecution` does with FFmpegKit.

use super::Reply;
use super::args::error;
use crate::manager::ExtensionManager;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Runs `operation` while a worker thread executes the FFmpeg commands the
/// backend queues for it. The worker finishes every command it claimed, even
/// after `operation` returns, because another operation on this owner may be
/// waiting for that result.
pub(super) fn with_ffmpeg(current: &ExtensionManager, operation: impl FnOnce() -> Reply) -> Reply {
    let commands = current.environment().ffmpeg_commands();
    let running = Arc::new(AtomicBool::new(true));
    let worker_running = Arc::clone(&running);
    std::thread::spawn(move || {
        while worker_running.load(Ordering::Acquire) {
            let Ok(pending) = commands.wait_pending(1_000) else {
                break;
            };
            for command in ffmpeg_commands(&pending) {
                let active = || {
                    commands
                        .get_command(command.id.clone())
                        .is_ok_and(|found| !found.is_empty())
                };
                let (success, output) = if !active() {
                    (false, "cancelled".to_owned())
                } else if command.arguments.is_empty() {
                    (false, "FFmpeg arguments are empty".to_owned())
                } else {
                    run_ffmpeg_command(&command, &active).unwrap_or_else(|message| (false, message))
                };
                let failure = if success {
                    String::new()
                } else {
                    output.clone()
                };
                // Owner shutdown removes commands; late results cannot revive them.
                let _ = commands.complete(command.id, success, output, failure);
            }
        }
    });
    let reply = operation();
    running.store(false, Ordering::Release);
    reply
}

pub(super) struct FfmpegCommand {
    pub(super) id: String,
    pub(super) arguments: Vec<String>,
    pub(super) output_path: String,
}

pub(super) fn ffmpeg_commands(raw: &str) -> Vec<FfmpegCommand> {
    let Ok(Value::Array(commands)) = serde_json::from_str(raw) else {
        return Vec::new();
    };
    commands
        .iter()
        .filter_map(|command| {
            let id = command.get("command_id")?.as_str()?.to_owned();
            if id.trim().is_empty() {
                return None;
            }
            let arguments = command
                .get("arguments")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(|value| value.as_str().unwrap_or("").to_owned())
                        .collect()
                })
                .unwrap_or_default();
            let output_path = command
                .get("output_path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            Some(FfmpegCommand {
                id,
                arguments,
                output_path,
            })
        })
        .collect()
}

/// Writes to a staged sibling and renames it over the target only after a
/// successful run, so an interrupted FFmpeg never leaves a partial file under
/// the final name.
pub(super) fn run_ffmpeg_command(
    command: &FfmpegCommand,
    active: &dyn Fn() -> bool,
) -> Result<(bool, String), String> {
    if command.output_path.is_empty() {
        return run_ffmpeg(&command.arguments, active);
    }
    if command.arguments.last() != Some(&command.output_path) {
        return Err("FFmpeg output does not match command".into());
    }
    let target = Path::new(&command.output_path);
    let staged = staged_ffmpeg_output(target)?;
    let mut arguments = command.arguments.clone();
    *arguments.last_mut().expect("arguments are not empty") = staged.to_string_lossy().into_owned();
    let result = run_ffmpeg(&arguments, active).and_then(|(success, output)| {
        if !active() {
            return Ok((false, "cancelled".to_owned()));
        }
        if success {
            std::fs::rename(&staged, target)
                .map_err(|e| format!("Failed to publish FFmpeg output: {e}"))?;
        }
        Ok((success, output))
    });
    let _ = std::fs::remove_file(&staged);
    result
}

fn staged_ffmpeg_output(target: &Path) -> Result<PathBuf, String> {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let directory = target.parent().ok_or("FFmpeg output has no directory")?;
    let extension = target
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
    let name = format!(
        ".mornye-ffmpeg-com.zarz.mornye-{}-{stamp}-{serial}.{extension}",
        std::process::id()
    );
    Ok(directory.join(name))
}

/// Runs `ffmpeg.exe` from the app folder or `PATH`, killing it if the backend
/// drops the command. Returns whether it succeeded and the tail of its log.
fn run_ffmpeg(arguments: &[String], active: &dyn Fn() -> bool) -> Result<(bool, String), String> {
    let bundled = std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name("ffmpeg.exe"))
        .filter(|path| path.is_file());
    let program = bundled.unwrap_or_else(|| PathBuf::from("ffmpeg"));
    let mut process = Command::new(&program);
    process
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        process.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = process
        .spawn()
        .map_err(|e| format!("FFmpeg could not start ({}): {e}", program.display()))?;
    // Drain the log on its own thread so a full pipe never stalls FFmpeg.
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let log = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr, &mut text);
        text
    });
    let status = loop {
        if let Some(status) = child.try_wait().map_err(error)? {
            break Some(status);
        }
        if !active() {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let log = log.join().unwrap_or_default();
    let tail: String = {
        let chars: Vec<char> = log.chars().collect();
        chars[chars.len().saturating_sub(4_000)..].iter().collect()
    };
    Ok(match status {
        None => (false, "cancelled".to_owned()),
        Some(status) => (status.success(), tail),
    })
}
