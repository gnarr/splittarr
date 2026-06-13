use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use anyhow::{anyhow, Result};
use rcue::parser::parse_from_file;
use regex::bytes::Regex;

use crate::application::ports::CueSplitter;
use crate::domain::{SplitOutcome, SplitStatus};

#[derive(Debug, Clone)]
pub struct ShnsplitCueSplitter {
    cue_strict: bool,
    shnsplit_path: PathBuf,
    overwrite: bool,
    format: String,
}

impl ShnsplitCueSplitter {
    pub fn new(cue_strict: bool, shnsplit_path: PathBuf, overwrite: bool, format: String) -> Self {
        Self {
            cue_strict,
            shnsplit_path,
            overwrite,
            format,
        }
    }
}

impl CueSplitter for ShnsplitCueSplitter {
    async fn split_cue(&self, cue_path: &Path) -> Result<SplitOutcome> {
        let splitter = self.clone();
        let cue_path = cue_path.to_path_buf();
        tokio::task::spawn_blocking(move || splitter.split_cue_sync(&cue_path))
            .await
            .map_err(|err| anyhow!("blocking task failed to join: {err}"))?
    }
}

impl ShnsplitCueSplitter {
    fn split_cue_sync(&self, cue_path: &Path) -> Result<SplitOutcome> {
        let cue_path_str = cue_path
            .to_str()
            .ok_or_else(|| anyhow!("cue path is not valid UTF-8: {}", cue_path.display()))?;
        let cue_dir = cue_path
            .parent()
            .ok_or_else(|| anyhow!("cue file has no parent directory: {}", cue_path.display()))?;
        let cue_file_name = cue_path
            .file_name()
            .ok_or_else(|| anyhow!("cue file name is not valid UTF-8: {}", cue_path.display()))?;

        let cue = parse_from_file(cue_path_str, self.cue_strict)
            .map_err(|err| anyhow!("failed to parse cue file {}: {err}", cue_path.display()))?;

        let expected_tracks = cue
            .files
            .iter()
            .map(|file| file.tracks.len())
            .sum::<usize>();

        let referenced_files = cue
            .files
            .iter()
            .map(|file| file.file.as_str())
            .filter(|file| cue_dir.join(file).exists())
            .collect::<BTreeSet<_>>();
        let referenced_paths = referenced_files
            .iter()
            .map(|file| cue_dir.join(file))
            .collect::<Vec<_>>();

        if referenced_files.is_empty() {
            return Ok(SplitOutcome {
                status: SplitStatus::Skipped,
                tracks: Vec::new(),
                message: Some("cue file does not reference an audio file in its directory".into()),
            });
        }

        let overwrite = if self.overwrite { "always" } else { "never" };
        let files_before = snapshot_audio_files_best_effort(cue_dir, cue_path);
        let output = {
            let mut command = Command::new(&self.shnsplit_path);
            command
                .current_dir(cue_dir)
                .arg("-f")
                .arg(cue_file_name)
                .arg("-d")
                .arg(cue_dir)
                .arg("-t")
                .arg(&self.format)
                .args(decoder_args(&referenced_paths))
                .arg("-O")
                .arg(overwrite)
                .arg("-o")
                .arg("flac flac -s -8 -o %f -");

            for file in referenced_files {
                command.arg(file);
            }

            command.output().map_err(|err| {
                anyhow!("failed to run shnsplit for {}: {err}", cue_path.display())
            })?
        };

        if !output.status.success() {
            let status = output.status.code().map_or_else(
                || "terminated by signal".to_owned(),
                |code| code.to_string(),
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "shnsplit failed for {} with status {status}: {stderr}",
                cue_path.display()
            ));
        }

        let parsed_tracks = parse_generated_tracks(cue_dir, &output.stderr);
        let files_after = snapshot_audio_files_best_effort(cue_dir, cue_path);
        let detected_tracks =
            detect_generated_tracks(&referenced_paths, &files_before, &files_after);
        let tracks = select_generated_tracks(
            cue_path,
            expected_tracks,
            &referenced_paths,
            parsed_tracks,
            detected_tracks,
            &files_after,
        )?;
        let tracks = normalize_generated_track_filenames(tracks)?;

        Ok(SplitOutcome {
            status: SplitStatus::Split,
            tracks,
            message: None,
        })
    }
}

fn select_generated_tracks(
    cue_path: &Path,
    expected_tracks: usize,
    referenced_paths: &[PathBuf],
    parsed_tracks: Vec<PathBuf>,
    detected_tracks: Vec<PathBuf>,
    files_after: &BTreeSet<FileSnapshot>,
) -> Result<Vec<PathBuf>> {
    let parsed_existing = existing_unique_tracks(parsed_tracks);
    if parsed_existing.len() == expected_tracks {
        return Ok(parsed_existing);
    }

    let combined_tracks =
        existing_unique_tracks(parsed_existing.into_iter().chain(detected_tracks).collect());
    if combined_tracks.len() == expected_tracks {
        return Ok(combined_tracks);
    }

    let directory_tracks = non_source_audio_tracks(referenced_paths, files_after);
    if directory_tracks.len() == expected_tracks {
        return Ok(directory_tracks);
    }

    let generated_tracks = combined_tracks.len().max(directory_tracks.len());
    if generated_tracks == 0 {
        return Err(anyhow!(
            "shnsplit succeeded for {} but no generated tracks were detected",
            cue_path.display()
        ));
    }

    Err(anyhow!(
        "shnsplit generated {} track(s) for {} but cue contains {} track(s)",
        generated_tracks,
        cue_path.display(),
        expected_tracks
    ))
}

fn existing_unique_tracks(tracks: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut unique = BTreeSet::new();
    tracks
        .into_iter()
        .filter(|track| track.exists())
        .filter(|track| unique.insert(track.clone()))
        .collect()
}

fn non_source_audio_tracks(
    referenced_paths: &[PathBuf],
    files_after: &BTreeSet<FileSnapshot>,
) -> Vec<PathBuf> {
    files_after
        .iter()
        .filter(|snapshot| !referenced_paths.iter().any(|path| path == &snapshot.path))
        .map(|snapshot| snapshot.path.clone())
        .collect()
}

fn parse_generated_tracks(cue_dir: &Path, stderr: &[u8]) -> Vec<PathBuf> {
    let re = Regex::new(
        r"Splitting \[(?P<input_file>[^]]+)] \((?P<input_length>[^)]+)\) --> \[(?P<output_file>[^]]+)] \((?P<output_length>[^)]+)\) :",
    )
    .expect("generated-track regex must compile");

    re.captures_iter(stderr)
        .filter_map(|cap| cap.name("output_file").map(|output| output.as_bytes()))
        .map(path_from_shnsplit_bytes)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                cue_dir.join(path)
            }
        })
        .collect()
}

fn path_from_shnsplit_bytes(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        PathBuf::from(OsString::from_vec(bytes.to_vec()))
    }

    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

fn normalize_generated_track_filenames(tracks: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    let plan = build_generated_track_rename_plan(tracks)?;
    for rename in &plan {
        if rename.source != rename.target {
            fs::rename(&rename.source, &rename.target).map_err(|err| {
                anyhow!(
                    "failed to rename generated track {} to {}: {err}",
                    rename.source.display(),
                    rename.target.display()
                )
            })?;
        }
    }
    Ok(plan.into_iter().map(|rename| rename.target).collect())
}

fn build_generated_track_rename_plan(tracks: Vec<PathBuf>) -> Result<Vec<GeneratedTrackRename>> {
    let mut plan = Vec::with_capacity(tracks.len());
    let mut targets = BTreeSet::new();

    for path in tracks {
        let target = sanitized_generated_track_path(&path)?;
        if !targets.insert(target.clone()) {
            return Err(anyhow!(
                "multiple generated tracks sanitize to the same path: {}",
                target.display()
            ));
        }
        if target.exists() && target != path {
            return Err(anyhow!(
                "sanitized generated track path already exists for {}: {}",
                path.display(),
                target.display()
            ));
        }
        plan.push(GeneratedTrackRename {
            source: path,
            target,
        });
    }

    Ok(plan)
}

fn sanitized_generated_track_path(path: &Path) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("generated track path has no file name: {}", path.display()))?;
    let sanitized = sanitize_file_name(file_name);
    if sanitized.is_empty() {
        return Err(anyhow!(
            "generated track filename sanitized to an empty name: {}",
            path.display()
        ));
    }

    Ok(path.with_file_name(sanitized))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GeneratedTrackRename {
    source: PathBuf,
    target: PathBuf,
}

fn sanitize_file_name(file_name: &std::ffi::OsStr) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        sanitize_file_name_bytes(file_name.as_bytes())
    }

    #[cfg(not(unix))]
    {
        sanitize_file_name_str(&file_name.to_string_lossy())
    }
}

#[cfg(unix)]
fn sanitize_file_name_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(value) => sanitize_file_name_str(value),
        Err(_) => sanitize_file_name_lossy_bytes(bytes),
    }
}

#[cfg(unix)]
fn sanitize_file_name_lossy_bytes(bytes: &[u8]) -> String {
    let mut sanitized = String::with_capacity(bytes.len());
    let mut previous_was_underscore = false;
    for byte in bytes {
        match ascii_replacement_for_byte(*byte) {
            ByteReplacement::Char(ch) => {
                sanitized.push(ch);
                previous_was_underscore = false;
            }
            ByteReplacement::Str(value) => {
                sanitized.push_str(value);
                previous_was_underscore = false;
            }
            ByteReplacement::Underscore => {
                push_collapsed_underscore(&mut sanitized, &mut previous_was_underscore)
            }
        }
    }
    trim_sanitized_file_name(sanitized)
}

fn sanitize_file_name_str(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    let mut previous_was_underscore = false;
    for ch in value.chars() {
        match ascii_replacement_for_char(ch) {
            CharReplacement::Char(ch) => {
                sanitized.push(ch);
                previous_was_underscore = false;
            }
            CharReplacement::Str(value) => {
                sanitized.push_str(value);
                previous_was_underscore = false;
            }
            CharReplacement::Underscore => {
                push_collapsed_underscore(&mut sanitized, &mut previous_was_underscore)
            }
        }
    }
    trim_sanitized_file_name(sanitized)
}

fn push_collapsed_underscore(value: &mut String, previous_was_underscore: &mut bool) {
    if !*previous_was_underscore {
        value.push('_');
        *previous_was_underscore = true;
    }
}

fn trim_sanitized_file_name(value: String) -> String {
    value.trim_matches(|ch| ch == ' ' || ch == '_').to_owned()
}

#[cfg(unix)]
enum ByteReplacement {
    Char(char),
    Str(&'static str),
    Underscore,
}

#[cfg(unix)]
fn ascii_replacement_for_byte(byte: u8) -> ByteReplacement {
    match byte {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b' ' | b'.' | b'-' | b'_' | b'\'' => {
            ByteReplacement::Char(char::from(byte))
        }
        0x91 | 0x92 => ByteReplacement::Char('\''),
        0x93 | 0x94 => ByteReplacement::Char('\''),
        0x96 | 0x97 => ByteReplacement::Char('-'),
        0x85 => ByteReplacement::Str("..."),
        _ => ByteReplacement::Underscore,
    }
}

enum CharReplacement {
    Char(char),
    Str(&'static str),
    Underscore,
}

fn ascii_replacement_for_char(ch: char) -> CharReplacement {
    match ch {
        'A'..='Z' | 'a'..='z' | '0'..='9' | ' ' | '.' | '-' | '_' | '\'' => {
            CharReplacement::Char(ch)
        }
        '\u{2018}' | '\u{2019}' => CharReplacement::Char('\''),
        '\u{201C}' | '\u{201D}' => CharReplacement::Char('\''),
        '\u{2013}' | '\u{2014}' => CharReplacement::Char('-'),
        '\u{2026}' => CharReplacement::Str("..."),
        _ => CharReplacement::Underscore,
    }
}

fn decoder_args(referenced_paths: &[PathBuf]) -> Vec<&'static str> {
    if referenced_paths.is_empty() || !referenced_paths.iter().all(|path| is_flac_file(path)) {
        return Vec::new();
    }
    vec!["-i", "flac flac -cd -s %f"]
}

fn detect_generated_tracks(
    referenced_paths: &[PathBuf],
    before: &BTreeSet<FileSnapshot>,
    after: &BTreeSet<FileSnapshot>,
) -> Vec<PathBuf> {
    after
        .iter()
        .filter(|snapshot| !referenced_paths.iter().any(|path| path == &snapshot.path))
        .filter(|snapshot| !before.contains(snapshot))
        .map(|snapshot| snapshot.path.clone())
        .collect()
}

fn snapshot_audio_files(root: &Path) -> Result<BTreeSet<FileSnapshot>> {
    let mut files = BTreeSet::new();
    for entry in fs::read_dir(root)
        .map_err(|err| anyhow!("failed to list cue directory {}: {err}", root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || !is_audio_file(&path) {
            continue;
        }

        let metadata = entry.metadata()?;
        files.insert(FileSnapshot {
            path,
            size: metadata.len(),
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        });
    }
    Ok(files)
}

fn snapshot_audio_files_best_effort(root: &Path, cue_path: &Path) -> BTreeSet<FileSnapshot> {
    match snapshot_audio_files(root) {
        Ok(files) => files,
        Err(err) => {
            eprintln!(
                "Unable to snapshot audio files for {}: {err}",
                cue_path.display()
            );
            BTreeSet::new()
        }
    }
}

fn is_flac_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("flac"))
}

fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "flac" | "wav" | "ape" | "wv" | "m4a" | "mp3" | "ogg" | "opus" | "tta" | "aiff"
            )
        })
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FileSnapshot {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    #[cfg(unix)]
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::{
        decoder_args, detect_generated_tracks, normalize_generated_track_filenames,
        parse_generated_tracks, sanitize_file_name_str, snapshot_audio_files_best_effort,
        ShnsplitCueSplitter,
    };
    use crate::domain::SplitStatus;

    #[test]
    fn splits_with_fake_shnsplit_and_records_absolute_tracks() {
        let tmp = tempdir().unwrap();
        let cue_path = write_fixture_album(tmp.path(), true);
        let fake = write_fake_shnsplit(
            tmp.path(),
            r#"touch "Artist - Album - 01 - Track One.flac"
echo "Splitting [album.flac] (0:01.00) --> [Artist - Album - 01 - Track One.flac] (0:01.00) :" >&2
exit 0
"#,
        );
        let splitter = test_splitter(fake);

        let result = splitter.split_cue_sync(&cue_path).unwrap();

        assert_eq!(result.status, SplitStatus::Split);
        assert_eq!(
            result.tracks,
            vec![tmp.path().join("Artist - Album - 01 - Track One.flac")]
        );
    }

    #[test]
    fn skips_when_cue_references_no_existing_audio() {
        let tmp = tempdir().unwrap();
        let cue_path = write_fixture_album(tmp.path(), false);
        let fake = write_fake_shnsplit(tmp.path(), "exit 1\n");
        let splitter = test_splitter(fake);

        let result = splitter.split_cue_sync(&cue_path).unwrap();

        assert_eq!(result.status, SplitStatus::Skipped);
        assert!(result.tracks.is_empty());
        assert!(result.message.unwrap().contains("does not reference"));
    }

    #[test]
    fn command_failure_is_an_error() {
        let tmp = tempdir().unwrap();
        let cue_path = write_fixture_album(tmp.path(), true);
        let fake = write_fake_shnsplit(
            tmp.path(),
            r#"echo "split failed" >&2
exit 2
"#,
        );
        let splitter = test_splitter(fake);

        let err = splitter.split_cue_sync(&cue_path).unwrap_err();

        assert!(err.to_string().contains("shnsplit failed"));
    }

    #[test]
    fn parses_generated_tracks_relative_to_cue_directory() {
        let tmp = tempdir().unwrap();
        let stderr = "Splitting [album.flac] (0:01.00) --> [01 - Track.flac] (0:01.00) :";

        let tracks = parse_generated_tracks(tmp.path(), stderr.as_bytes());

        assert_eq!(tracks, vec![tmp.path().join("01 - Track.flac")]);
    }

    #[cfg(unix)]
    #[test]
    fn splitter_sanitizes_non_utf8_track_paths_from_stderr() {
        let tmp = tempdir().unwrap();
        let cue_path = write_fixture_album(tmp.path(), true);
        let fake = write_fake_shnsplit(
            tmp.path(),
            r#"name=$(printf 'Artist - Album - 01 - You\222re Lost.flac')
printf 'track' > "$name"
printf 'Splitting [album.flac] (0:01.00) --> [%s] (0:01.00) :\n' "$name" >&2
exit 0
"#,
        );
        let splitter = test_splitter(fake);
        let raw_path = tmp.path().join(PathBuf::from(OsString::from_vec(
            b"Artist - Album - 01 - You\x92re Lost.flac".to_vec(),
        )));
        let expected = tmp.path().join("Artist - Album - 01 - You're Lost.flac");

        let result = splitter.split_cue_sync(&cue_path).unwrap();

        assert_eq!(result.status, SplitStatus::Split);
        assert_eq!(result.tracks, vec![expected]);
        assert!(result.tracks[0].exists());
        assert!(!raw_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn splitter_reconciles_directory_tracks_when_detection_misses_non_utf8_names() {
        let tmp = tempdir().unwrap();
        let cue_path = write_ten_track_fixture_album(tmp.path());
        let raw_tracks = [
            b"Artist - Album - 02 - You\x92re Lost.flac".to_vec(),
            b"Artist - Album - 09 - I Can\x92t See.flac".to_vec(),
            b"Artist - Album - 10 - Music\x92s Over.flac".to_vec(),
        ];
        let raw_paths = raw_tracks
            .iter()
            .map(|name| {
                tmp.path()
                    .join(PathBuf::from(OsString::from_vec(name.clone())))
            })
            .collect::<Vec<_>>();
        for path in &raw_paths {
            fs::write(path, b"track").unwrap();
        }
        let fake = write_fake_shnsplit(
            tmp.path(),
            r#"for n in 01 03 04 05 06 07 08; do
  track="Artist - Album - $n - Track $n.flac"
  printf 'track' > "$track"
  printf 'Splitting [album.flac] (0:01.00) --> [%s] (0:01.00) :\n' "$track" >&2
done
exit 0
"#,
        );
        let splitter = test_splitter(fake);

        let result = splitter.split_cue_sync(&cue_path).unwrap();

        assert_eq!(result.status, SplitStatus::Split);
        assert_eq!(result.tracks.len(), 10);
        assert_eq!(
            result.tracks,
            vec![
                tmp.path().join("Artist - Album - 01 - Track 01.flac"),
                tmp.path().join("Artist - Album - 02 - You're Lost.flac"),
                tmp.path().join("Artist - Album - 03 - Track 03.flac"),
                tmp.path().join("Artist - Album - 04 - Track 04.flac"),
                tmp.path().join("Artist - Album - 05 - Track 05.flac"),
                tmp.path().join("Artist - Album - 06 - Track 06.flac"),
                tmp.path().join("Artist - Album - 07 - Track 07.flac"),
                tmp.path().join("Artist - Album - 08 - Track 08.flac"),
                tmp.path().join("Artist - Album - 09 - I Can't See.flac"),
                tmp.path().join("Artist - Album - 10 - Music's Over.flac"),
            ]
        );
        for track in &result.tracks {
            assert!(track.exists());
        }
        for path in &raw_paths {
            assert!(!path.exists());
        }
    }

    #[test]
    fn sanitizes_unicode_punctuation_and_remaining_non_ascii() {
        let sanitized = sanitize_file_name_str(
            "Artist - Album - 02 - \u{201C}Cafe\u{201D}\u{2014}deja vu\u{2026}.flac",
        );

        assert_eq!(sanitized, "Artist - Album - 02 - 'Cafe'-deja vu....flac");
    }

    #[cfg(unix)]
    #[test]
    fn sanitizes_windows_1252_curly_double_quotes_consistently() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let sanitized = super::sanitize_file_name(OsStr::from_bytes(
            b"Artist - Album - 02 - \x93Cafe\x94.flac",
        ));

        assert_eq!(sanitized, "Artist - Album - 02 - 'Cafe'.flac");
    }

    #[test]
    fn sanitizer_collapses_underscores_and_trims_edges() {
        let sanitized = sanitize_file_name_str("  \u{00E9}\u{00E5} Track \u{266B}.flac  ");

        assert_eq!(sanitized, "Track _.flac");
    }

    #[test]
    fn generated_track_rename_preflight_rejects_duplicate_sanitized_targets() {
        let tmp = tempdir().unwrap();
        let first = tmp.path().join("Artist - Album - 01 - caf\u{00E9}.flac");
        let second = tmp.path().join("Artist - Album - 01 - caf\u{00E5}.flac");
        let target = tmp.path().join("Artist - Album - 01 - caf_.flac");
        fs::write(&first, "first").unwrap();
        fs::write(&second, "second").unwrap();

        let err =
            normalize_generated_track_filenames(vec![first.clone(), second.clone()]).unwrap_err();

        assert!(err
            .to_string()
            .contains("multiple generated tracks sanitize to the same path"));
        assert!(first.exists());
        assert!(second.exists());
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn splitter_fails_when_sanitized_track_path_collides() {
        let tmp = tempdir().unwrap();
        let cue_path = write_fixture_album(tmp.path(), true);
        fs::write(
            tmp.path().join("Artist - Album - 01 - You're Lost.flac"),
            "existing",
        )
        .unwrap();
        let fake = write_fake_shnsplit(
            tmp.path(),
            r#"name=$(printf 'Artist - Album - 01 - You\222re Lost.flac')
printf 'track' > "$name"
printf 'Splitting [album.flac] (0:01.00) --> [%s] (0:01.00) :\n' "$name" >&2
exit 0
"#,
        );
        let splitter = test_splitter(fake);

        let err = splitter.split_cue_sync(&cue_path).unwrap_err();

        assert!(err
            .to_string()
            .contains("sanitized generated track path already exists"));
    }

    #[test]
    fn falls_back_to_filesystem_detection_when_stderr_has_no_track_lines() {
        let tmp = tempdir().unwrap();
        let input = tmp.path().join("album.flac");
        let output = tmp.path().join("01 - Track.flac");
        fs::write(&input, b"input").unwrap();
        let before = super::snapshot_audio_files(tmp.path()).unwrap();
        fs::write(&output, b"track").unwrap();
        let after = super::snapshot_audio_files(tmp.path()).unwrap();

        let tracks = detect_generated_tracks(&[input], &before, &after);

        assert_eq!(tracks, vec![output]);
    }

    #[test]
    fn uses_explicit_flac_decoder_for_flac_inputs() {
        let args = decoder_args(&[PathBuf::from("album.flac")]);

        assert_eq!(args, vec!["-i", "flac flac -cd -s %f"]);
    }

    #[test]
    fn splitter_detects_tracks_without_matching_stderr() {
        let tmp = tempdir().unwrap();
        let cue_path = write_fixture_album(tmp.path(), true);
        let fake = write_fake_shnsplit(
            tmp.path(),
            r#"touch "Artist - Album - 01 - Track One.flac"
echo "split complete" >&2
exit 0
"#,
        );
        let splitter = test_splitter(fake);

        let result = splitter.split_cue_sync(&cue_path).unwrap();

        assert_eq!(result.status, SplitStatus::Split);
        assert_eq!(
            result.tracks,
            vec![tmp.path().join("Artist - Album - 01 - Track One.flac")]
        );
    }

    #[test]
    fn splitter_rejects_partial_generated_track_detection() {
        let tmp = tempdir().unwrap();
        let cue_path = write_two_track_fixture_album(tmp.path());
        let fake = write_fake_shnsplit(
            tmp.path(),
            r#"touch "Artist - Album - 01 - Track One.flac"
echo "split complete" >&2
exit 0
"#,
        );
        let splitter = test_splitter(fake);

        let err = splitter.split_cue_sync(&cue_path).unwrap_err();

        assert!(err.to_string().contains("shnsplit generated 1 track(s)"));
        assert!(err.to_string().contains("cue contains 2 track(s)"));
    }

    #[test]
    fn splitter_passes_flac_decoder_argument() {
        let tmp = tempdir().unwrap();
        let cue_path = write_fixture_album(tmp.path(), true);
        let args_log = tmp.path().join("args.log");
        let fake = write_fake_shnsplit(
            tmp.path(),
            &format!(
                "printf '%s\\n' \"$@\" > \"{}\"\ntouch \"Artist - Album - 01 - Track One.flac\"\necho \"Splitting [album.flac] (0:01.00) --> [Artist - Album - 01 - Track One.flac] (0:01.00) :\" >&2\nexit 0\n",
                args_log.display()
            ),
        );
        let splitter = test_splitter(fake);

        splitter.split_cue_sync(&cue_path).unwrap();
        let args = fs::read_to_string(args_log).unwrap();

        assert!(args.contains("-i"));
        assert!(args.contains("flac flac -cd -s %f"));
    }

    #[test]
    fn best_effort_snapshot_failure_does_not_block_stderr_track_detection() {
        let tmp = tempdir().unwrap();
        let cue_path = tmp.path().join("album.cue");
        let missing_dir = tmp.path().join("missing");

        let files = snapshot_audio_files_best_effort(&missing_dir, &cue_path);

        assert!(files.is_empty());
    }

    fn test_splitter(shnsplit_path: PathBuf) -> ShnsplitCueSplitter {
        ShnsplitCueSplitter::new(true, shnsplit_path, true, "%p - %a - %n - %t".into())
    }

    fn write_fixture_album(dir: &Path, with_audio: bool) -> PathBuf {
        let cue_path = dir.join("album.cue");
        fs::write(
            &cue_path,
            r#"PERFORMER "Artist"
TITLE "Album"
FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Track One"
    PERFORMER "Artist"
    INDEX 01 00:00:00
"#,
        )
        .unwrap();
        if with_audio {
            fs::write(dir.join("album.flac"), "").unwrap();
        }
        cue_path
    }

    fn write_two_track_fixture_album(dir: &Path) -> PathBuf {
        let cue_path = dir.join("album.cue");
        fs::write(
            &cue_path,
            r#"PERFORMER "Artist"
TITLE "Album"
FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Track One"
    PERFORMER "Artist"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Track Two"
    PERFORMER "Artist"
    INDEX 01 01:00:00
"#,
        )
        .unwrap();
        fs::write(dir.join("album.flac"), "").unwrap();
        cue_path
    }

    fn write_ten_track_fixture_album(dir: &Path) -> PathBuf {
        let cue_path = dir.join("album.cue");
        let mut cue = String::from(
            r#"PERFORMER "Artist"
TITLE "Album"
FILE "album.flac" WAVE
"#,
        );
        for track in 1..=10 {
            cue.push_str(&format!(
                r#"  TRACK {track:02} AUDIO
    TITLE "Track {track:02}"
    PERFORMER "Artist"
    INDEX 01 {track:02}:00:00
"#
            ));
        }
        fs::write(&cue_path, cue).unwrap();
        fs::write(dir.join("album.flac"), "").unwrap();
        cue_path
    }

    fn write_fake_shnsplit(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("fake-shnsplit");
        fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).unwrap();
        }
        path
    }
}
