use super::*;
use std::io::Write;
#[cfg(unix)]
use std::io::{Seek, SeekFrom};

#[cfg(unix)]
#[test]
fn native_reads_reject_links_at_every_component_and_never_write_or_wait_on_special_files() {
    use std::io::Read;
    use std::os::unix::{fs::symlink, net::UnixListener};

    let root = tempfile::tempdir().unwrap();
    let files = Arc::new(FileRegistry::default())
        .extension(root.path())
        .unwrap();
    std::fs::create_dir_all(root.path().join("album/disc")).unwrap();
    std::fs::write(root.path().join("album/disc/song.wav"), b"original").unwrap();
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::unix::fs::PermissionsExt;
        let parent = root.path().join("album");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o111)).unwrap();
        let result = files
            .resolve("album/disc/song.wav")
            .unwrap()
            .open_native_read();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_ok(), "execute-only parent: {result:?}");
    }
    let mut file = files
        .resolve("album/disc/song.wav")
        .unwrap()
        .open_native_read()
        .unwrap();
    let mut data = Vec::new();
    file.read_to_end(&mut data).unwrap();
    assert_eq!(data, b"original");
    assert!(file.write_all(b"changed").is_err());
    for (target, link) in [
        ("album", "album-link"),
        ("album/disc/song.wav", "song-link.wav"),
        ("missing.wav", "dangling.wav"),
    ] {
        symlink(target, root.path().join(link)).unwrap();
    }
    for path in ["album-link/disc/song.wav", "song-link.wav", "dangling.wav"] {
        assert!(
            files
                .resolve(path)
                .unwrap()
                .open_native_read()
                .unwrap_err()
                .to_string()
                .contains("symlinks")
        );
    }
    assert_eq!(
        files
            .resolve("missing.wav")
            .unwrap()
            .open_native_read()
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert!(
        std::process::Command::new("mkfifo")
            .arg(root.path().join("pipe.wav"))
            .status()
            .unwrap()
            .success()
    );
    let _socket = UnixListener::bind(root.path().join("socket.wav")).unwrap();
    for path in ["pipe.wav", "socket.wav"] {
        assert!(
            files
                .resolve(path)
                .unwrap()
                .open_native_read()
                .unwrap_err()
                .to_string()
                .contains("special files")
        );
    }
    assert_eq!(
        std::fs::read(root.path().join("album/disc/song.wav")).unwrap(),
        b"original"
    );
}

#[test]
fn exclusive_publication_preserves_racing_output_and_cleans_cancelled_stage() {
    let root = tempfile::tempdir().unwrap();
    let files = Arc::new(FileRegistry::default())
        .extension(root.path())
        .unwrap();
    let path = files.resolve("track.flac").unwrap();
    let mut stage = path.stage().unwrap();
    stage.file.write_all(b"new audio").unwrap();
    assert!(!path.absolute.exists());
    let error = stage
        .publish_new(&|| {
            // A different publisher creates the destination after the
            // planner's existence check but before the publication syscall.
            std::fs::write(&path.absolute, b"other publisher").unwrap();
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&path.absolute).unwrap(), b"other publisher");

    let cancelled = files.resolve("cancelled.flac").unwrap();
    let mut stage = cancelled.stage().unwrap();
    stage.file.write_all(b"cancelled audio").unwrap();
    assert_eq!(
        stage
            .publish_new(&|| Err("cancelled".into()))
            .unwrap_err()
            .to_string(),
        "cancelled"
    );
    assert!(!cancelled.absolute.exists());

    path.remove().unwrap();
    let mut stage = path.stage().unwrap();
    stage.file.write_all(b"complete audio").unwrap();
    stage.publish_new(&|| Ok(())).unwrap();
    assert_eq!(std::fs::read(&path.absolute).unwrap(), b"complete audio");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn stage_from_links_same_filesystem_source_and_survives_source_cleanup() {
    use cap_std::fs::MetadataExt;

    let root = tempfile::tempdir().unwrap();
    let files = Arc::new(FileRegistry::default())
        .extension(root.path())
        .unwrap();
    let source_path = files.resolve("source.bin").unwrap();
    std::fs::write(&source_path.absolute, b"source audio").unwrap();
    let source = source_path.open(OpenOptions::new().read(true)).unwrap();
    let destination = files.resolve("destination.bin").unwrap();
    let (stage, promoted) = destination.stage_from(&source_path, &source).unwrap();
    assert!(promoted);
    let source_metadata = source.metadata().unwrap();
    let stage_metadata = stage.file.metadata().unwrap();
    assert_eq!(source_metadata.dev(), stage_metadata.dev());
    assert_eq!(source_metadata.ino(), stage_metadata.ino());

    stage.publish_new(&|| Ok(())).unwrap();
    drop(source);
    source_path.remove().unwrap();
    assert_eq!(
        std::fs::read(&destination.absolute).unwrap(),
        b"source audio"
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn stage_from_copies_open_source_after_path_replacement() {
    let root = tempfile::tempdir().unwrap();
    let files = Arc::new(FileRegistry::default())
        .extension(root.path())
        .unwrap();
    let source_path = files.resolve("source.bin").unwrap();
    std::fs::write(&source_path.absolute, b"original audio").unwrap();
    let mut source = source_path.open(OpenOptions::new().read(true)).unwrap();
    std::fs::rename(&source_path.absolute, root.path().join("source.old")).unwrap();
    std::fs::write(&source_path.absolute, b"replacement audio").unwrap();
    let destination = files.resolve("destination.bin").unwrap();
    let (mut stage, promoted) = destination.stage_from(&source_path, &source).unwrap();
    assert!(!promoted);
    source.seek(SeekFrom::Start(0)).unwrap();
    std::io::copy(&mut source, &mut stage.file).unwrap();
    stage.publish_new(&|| Ok(())).unwrap();
    assert_eq!(
        std::fs::read(&destination.absolute).unwrap(),
        b"original audio"
    );
}

#[cfg(unix)]
#[test]
fn stage_from_copies_open_source_after_path_becomes_symlink() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let files = Arc::new(FileRegistry::default())
        .extension(root.path())
        .unwrap();
    let source_path = files.resolve("source.bin").unwrap();
    std::fs::write(&source_path.absolute, b"original audio").unwrap();
    let mut source = source_path.open(OpenOptions::new().read(true)).unwrap();
    std::fs::rename(&source_path.absolute, root.path().join("source.old")).unwrap();
    symlink("source.old", &source_path.absolute).unwrap();
    let destination = files.resolve("destination.bin").unwrap();
    let (mut stage, promoted) = destination.stage_from(&source_path, &source).unwrap();
    assert!(!promoted);
    source.seek(SeekFrom::Start(0)).unwrap();
    std::io::copy(&mut source, &mut stage.file).unwrap();
    stage.publish_new(&|| Ok(())).unwrap();
    assert_eq!(
        std::fs::read(&destination.absolute).unwrap(),
        b"original audio"
    );
}

#[cfg(unix)]
#[test]
fn stage_from_preserves_existing_destination_and_cleans_cancelled_stage() {
    let root = tempfile::tempdir().unwrap();
    let files = Arc::new(FileRegistry::default())
        .extension(root.path())
        .unwrap();
    let source_path = files.resolve("source.bin").unwrap();
    std::fs::write(&source_path.absolute, b"source audio").unwrap();
    let source = source_path.open(OpenOptions::new().read(true)).unwrap();
    let destination = files.resolve("destination.bin").unwrap();
    std::fs::write(&destination.absolute, b"existing audio").unwrap();

    let (stage, promoted) = destination.stage_from(&source_path, &source).unwrap();
    assert!(promoted);
    let error = stage.publish_new(&|| Ok(())).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read(&destination.absolute).unwrap(),
        b"existing audio"
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);

    std::fs::remove_file(&destination.absolute).unwrap();
    let (stage, promoted) = destination.stage_from(&source_path, &source).unwrap();
    assert!(promoted);
    let error = stage.publish_new(&|| Err("cancelled".into())).unwrap_err();
    assert_eq!(error.to_string(), "cancelled");
    assert!(!destination.absolute.exists());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}
