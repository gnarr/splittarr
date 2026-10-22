use std::fmt;
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use encoding_rs::{Encoding, UTF_16BE, UTF_16LE, UTF_8};
use rcue::cue::Cue;
use rcue::parser::parse;
use tempfile::{Builder, TempPath};

pub(crate) const TEMPORARY_CUE_PREFIX: &str = ".splittarr-cue-";
pub(crate) const DEFAULT_LEGACY_ENCODING_LABELS: &[&str] = &["windows-1251", "windows-1250"];

#[derive(Clone)]
pub(crate) struct CueReader {
    legacy_encodings: Arc<[&'static Encoding]>,
}

impl CueReader {
    pub fn new<I, S>(labels: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut legacy_encodings: Vec<&'static Encoding> = Vec::new();
        for label in labels {
            let label = label.as_ref().trim();
            let encoding = Encoding::for_label_no_replacement(label.as_bytes())
                .ok_or_else(|| anyhow!("unsupported CUE legacy encoding label: {label:?}"))?;
            if encoding == UTF_8 || encoding == UTF_16LE || encoding == UTF_16BE {
                return Err(anyhow!(
                    "CUE legacy encoding {label:?} resolves to {}; Unicode encodings cannot be configured as legacy fallbacks",
                    encoding.name()
                ));
            }
            if !legacy_encodings
                .iter()
                .any(|candidate| std::ptr::eq(*candidate, encoding))
            {
                legacy_encodings.push(encoding);
            }
        }

        Ok(Self {
            legacy_encodings: legacy_encodings.into(),
        })
    }

    pub fn read(&self, cue_path: &Path, strict: bool) -> Result<Cue> {
        let cue_dir = cue_directory(cue_path)?;
        Ok(self.decode(cue_path, cue_dir, strict)?.cue)
    }

    pub fn prepare(&self, cue_path: &Path, cue_dir: &Path, strict: bool) -> Result<PreparedCue> {
        let decoded = self.decode(cue_path, cue_dir, strict)?;
        let Some(legacy_contents) = decoded.legacy_contents else {
            return Ok(PreparedCue {
                cue: decoded.cue,
                command_path: cue_path.to_path_buf(),
                _temporary_path: None,
            });
        };

        let temporary_path =
            write_temporary_utf8_cue(cue_dir, legacy_contents.as_bytes(), cue_path)?;
        let command_path = temporary_path.to_path_buf();
        Ok(PreparedCue {
            cue: decoded.cue,
            command_path,
            _temporary_path: Some(temporary_path),
        })
    }

    fn decode(&self, cue_path: &Path, cue_dir: &Path, strict: bool) -> Result<DecodedCue> {
        let bytes = fs::read(cue_path)
            .map_err(|err| anyhow!("failed to read cue file {}: {err}", cue_path.display()))?;

        match std::str::from_utf8(&bytes) {
            Ok(decoded) => {
                let cue = parse_decoded_cue(decoded, strict).map_err(|err| {
                    anyhow!("failed to parse cue file {}: {err}", cue_path.display())
                })?;
                Ok(DecodedCue {
                    cue,
                    legacy_contents: None,
                })
            }
            Err(utf8_error) => self.decode_legacy(cue_path, cue_dir, strict, &bytes, utf8_error),
        }
    }

    fn decode_legacy(
        &self,
        cue_path: &Path,
        cue_dir: &Path,
        strict: bool,
        bytes: &[u8],
        utf8_error: std::str::Utf8Error,
    ) -> Result<DecodedCue> {
        let mut failures = Vec::new();
        let mut candidates = Vec::new();

        for &encoding in self.legacy_encodings.as_ref() {
            let label = encoding.name();
            let (decoded, had_errors) = encoding.decode_without_bom_handling(bytes);
            if had_errors {
                failures.push(format!("{label}: decoding required replacement characters"));
                continue;
            }

            let cue = match parse_decoded_cue(&decoded, strict) {
                Ok(cue) => cue,
                Err(err) => {
                    failures.push(format!("{label}: parse failed: {err}"));
                    continue;
                }
            };

            if cue.files.is_empty() {
                failures.push(format!("{label}: parsed no FILE references"));
                continue;
            }

            let missing_files = missing_referenced_files(&cue, cue_dir);
            if !missing_files.is_empty() {
                failures.push(format!(
                    "{label}: referenced files do not exist: {}",
                    missing_files.join(", ")
                ));
                continue;
            }

            candidates.push(LegacyCandidate {
                label,
                cue,
                contents: decoded.into_owned(),
            });
        }

        if candidates.is_empty() {
            let failures = if failures.is_empty() {
                "no legacy encodings are configured".to_owned()
            } else {
                failures.join("; ")
            };
            return Err(anyhow!(
                "failed to decode legacy cue file {}: input is not valid UTF-8 ({utf8_error}); no candidate decoding resolved all FILE references ({failures})",
                cue_path.display()
            ));
        }

        if candidates.len() > 1
            && candidates
                .iter()
                .skip(1)
                .any(|candidate| candidate.contents != candidates[0].contents)
        {
            let labels = candidates
                .iter()
                .map(|candidate| candidate.label)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(anyhow!(
                "ambiguous legacy cue encoding for {}: {labels} all resolve every FILE reference but produce different text",
                cue_path.display()
            ));
        }

        let candidate = candidates.remove(0);
        eprintln!(
            "Decoded legacy CUE {} using {}",
            cue_path.display(),
            candidate.label
        );
        Ok(DecodedCue {
            cue: candidate.cue,
            legacy_contents: Some(candidate.contents),
        })
    }
}

impl Default for CueReader {
    fn default() -> Self {
        Self::new(DEFAULT_LEGACY_ENCODING_LABELS)
            .expect("default CUE legacy encoding labels must be valid")
    }
}

impl fmt::Debug for CueReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = self
            .legacy_encodings
            .iter()
            .map(|encoding| encoding.name())
            .collect::<Vec<_>>();
        formatter
            .debug_struct("CueReader")
            .field("legacy_encodings", &names)
            .finish()
    }
}

pub(crate) struct PreparedCue {
    pub cue: Cue,
    pub command_path: PathBuf,
    _temporary_path: Option<TempPath>,
}

struct DecodedCue {
    cue: Cue,
    legacy_contents: Option<String>,
}

struct LegacyCandidate {
    label: &'static str,
    cue: Cue,
    contents: String,
}

fn cue_directory(cue_path: &Path) -> Result<&Path> {
    cue_path
        .parent()
        .ok_or_else(|| anyhow!("cue file has no parent directory: {}", cue_path.display()))
}

fn parse_decoded_cue(
    decoded: &str,
    strict: bool,
) -> std::result::Result<Cue, rcue::errors::CueError> {
    let mut reader = Cursor::new(decoded.as_bytes());
    parse(&mut reader, strict)
}

fn missing_referenced_files(cue: &Cue, cue_dir: &Path) -> Vec<String> {
    cue.files
        .iter()
        .map(|file| file.file.as_str())
        .filter(|file| !cue_dir.join(file).exists())
        .map(str::to_owned)
        .collect()
}

fn write_temporary_utf8_cue(
    cue_dir: &Path,
    decoded: &[u8],
    original_cue_path: &Path,
) -> Result<TempPath> {
    let mut temporary = Builder::new()
        .prefix(TEMPORARY_CUE_PREFIX)
        .suffix(".cue")
        .tempfile_in(cue_dir)
        .map_err(|err| {
            anyhow!(
                "failed to create temporary UTF-8 cue for {}: {err}",
                original_cue_path.display()
            )
        })?;
    temporary.write_all(decoded).map_err(|err| {
        anyhow!(
            "failed to write temporary UTF-8 cue for {}: {err}",
            original_cue_path.display()
        )
    })?;
    temporary.flush().map_err(|err| {
        anyhow!(
            "failed to flush temporary UTF-8 cue for {}: {err}",
            original_cue_path.display()
        )
    })?;
    Ok(temporary.into_temp_path())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::CueReader;

    const WINDOWS_1252_CUE: &[u8] = b"TITLE \"Can\xE7\xE3o\"\n\
FILE \"can\xE7\xE3o.flac\" WAVE\n\
  TRACK 01 AUDIO\n\
    TITLE \"Faixa\"\n\
    INDEX 01 00:00:00\n";

    #[test]
    fn accepts_encoding_rs_labels_and_deduplicates_aliases() {
        let reader = CueReader::new(["windows-1252", "cp1252"]).unwrap();

        assert_eq!(
            format!("{reader:?}"),
            "CueReader { legacy_encodings: [\"windows-1252\"] }"
        );
    }

    #[test]
    fn rejects_unsupported_encoding_label() {
        let err = CueReader::new(["not-an-encoding"]).unwrap_err();

        assert!(err
            .to_string()
            .contains("unsupported CUE legacy encoding label: \"not-an-encoding\""));
    }

    #[test]
    fn rejects_unicode_encoding_as_legacy_fallback() {
        let err = CueReader::new(["utf-8"]).unwrap_err();

        assert!(err
            .to_string()
            .contains("Unicode encodings cannot be configured as legacy fallbacks"));
    }

    #[test]
    fn reads_cue_with_configured_encoding_outside_defaults() {
        let tmp = tempdir().unwrap();
        let cue_path = tmp.path().join("western.cue");
        fs::write(&cue_path, WINDOWS_1252_CUE).unwrap();
        fs::write(tmp.path().join("canção.flac"), b"audio").unwrap();

        let default_error = CueReader::default().read(&cue_path, true).unwrap_err();
        assert!(default_error
            .to_string()
            .contains("no candidate decoding resolved all FILE references"));

        let cue = CueReader::new(["windows-1252"])
            .unwrap()
            .read(&cue_path, true)
            .unwrap();

        assert_eq!(cue.title.as_deref(), Some("Canção"));
        assert_eq!(cue.files[0].file, "canção.flac");
        assert_eq!(fs::read(&cue_path).unwrap(), WINDOWS_1252_CUE);
    }

    #[test]
    fn reports_when_legacy_fallbacks_are_disabled() {
        let tmp = tempdir().unwrap();
        let cue_path = tmp.path().join("western.cue");
        fs::write(&cue_path, WINDOWS_1252_CUE).unwrap();

        let err = CueReader::new(std::iter::empty::<&str>())
            .unwrap()
            .read(&cue_path, true)
            .unwrap_err();

        assert!(err
            .to_string()
            .contains("no legacy encodings are configured"));
    }

    #[test]
    fn rejects_legacy_cue_when_multiple_encodings_resolve_the_same_ascii_file() {
        let tmp = tempdir().unwrap();
        let cue_path = tmp.path().join("ambiguous.cue");
        fs::write(
            &cue_path,
            b"TITLE \"\xC0lbum\"\nFILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"Track\"\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        fs::write(tmp.path().join("album.flac"), b"audio").unwrap();

        let err = CueReader::default().read(&cue_path, true).unwrap_err();
        let message = err.to_string();

        assert!(message.contains("ambiguous legacy cue encoding"));
        assert!(message.contains("windows-1251"));
        assert!(message.contains("windows-1250"));
        assert!(message.contains("resolve every FILE reference"));
    }
}
