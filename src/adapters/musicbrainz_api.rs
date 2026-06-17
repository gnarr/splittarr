use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use base64::Engine;
use rcue::parser::parse_from_file;
use reqwest::Client;
use serde::Deserialize;
use sha1::{Digest, Sha1};
use tokio::time::sleep;

use crate::application::ports::{
    MusicBrainzDiscLookupRequest, MusicBrainzDiscLookupResult, MusicBrainzDiscRelease,
    MusicBrainzDiscReleaseLookup,
};
use crate::bootstrap::settings::MusicBrainzSettings;

const CD_FRAMES_PER_SECOND: u64 = 75;
const CD_LEAD_IN_FRAMES: u64 = 150;
const CD_SAMPLE_RATE: u64 = 44_100;
const SPLITTARR_USER_AGENT: &str = concat!("Splittarr/", env!("CARGO_PKG_VERSION"));
const MUSICBRAINZ_DISC_ID_INC: &str = "artists+recordings+release-groups";

#[derive(Debug, Clone)]
pub struct FilesystemMusicBrainzDiscReleaseLookup {
    enabled: bool,
    base_url: String,
    client: Client,
    rate_limiter: Arc<MusicBrainzRateLimiter>,
}

impl FilesystemMusicBrainzDiscReleaseLookup {
    pub fn new(settings: &MusicBrainzSettings) -> Self {
        Self {
            enabled: settings.disc_lookup_enabled,
            base_url: settings.base_url.trim_end_matches('/').to_owned(),
            client: Client::new(),
            rate_limiter: global_rate_limiter(),
        }
    }

    #[cfg(test)]
    fn with_rate_limiter(mut self, rate_limiter: Arc<MusicBrainzRateLimiter>) -> Self {
        self.rate_limiter = rate_limiter;
        self
    }

    async fn lookup_releases(
        &self,
        toc: &MusicBrainzToc,
        diagnostic: &mut String,
    ) -> Result<Vec<MusicBrainzDiscRelease>> {
        let path = format!("/ws/2/discid/{}", toc.disc_id);
        let url = format!("{}{}", self.base_url, path);
        diagnostic.push_str(&format!(
            "MusicBrainz request: path={} query=toc={}&inc={}&fmt=json\n",
            path, toc.toc, MUSICBRAINZ_DISC_ID_INC
        ));

        self.rate_limiter.until_ready().await;
        let response = self
            .client
            .get(url)
            .query(&[
                ("toc", toc.toc.as_str()),
                ("inc", MUSICBRAINZ_DISC_ID_INC),
                ("fmt", "json"),
            ])
            .header("user-agent", SPLITTARR_USER_AGENT)
            .send()
            .await
            .map_err(|err| anyhow!("failed requesting MusicBrainz: {err}"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| anyhow!("failed reading MusicBrainz response: {err}"))?;
        if !status.is_success() {
            return Err(anyhow!("MusicBrainz returned HTTP {status}: {body}"));
        }

        let response: MusicBrainzDiscIdResponse = serde_json::from_str(&body)
            .map_err(|err| anyhow!("MusicBrainz returned invalid JSON: {err}; body: {body}"))?;
        let releases = response
            .releases
            .into_iter()
            .filter(|release| !release.id.trim().is_empty())
            .map(|release| MusicBrainzDiscRelease {
                id: release.id,
                title: release.title,
                date: release.date,
                country: release.country,
                status: release.status,
                barcode: release.barcode.filter(|barcode| !barcode.trim().is_empty()),
                quality: release.quality,
                media_count: release.media.len(),
                media_formats: release
                    .media
                    .iter()
                    .filter_map(|medium| medium.format.clone())
                    .collect(),
                media_track_counts: release
                    .media
                    .iter()
                    .filter_map(|medium| medium.track_count)
                    .collect(),
                label_count: release.label_info.len(),
                release_group_id: release
                    .release_group
                    .as_ref()
                    .and_then(|release_group| release_group.id.clone())
                    .filter(|id| !id.trim().is_empty()),
                release_group_title: release
                    .release_group
                    .as_ref()
                    .and_then(|release_group| release_group.title.clone()),
                release_group_first_release_date: release
                    .release_group
                    .as_ref()
                    .and_then(|release_group| release_group.first_release_date.clone()),
            })
            .collect::<Vec<_>>();
        for release in &releases {
            diagnostic.push_str(&format!(
                "MusicBrainz response release: id={} title={} date={} country={} status={} barcode_present={} quality={} media_count={} formats=[{}] track_counts=[{}] labels={} release_group_id={} release_group={} release_group_first_release_date={}\n",
                release.id,
                release.title.as_deref().unwrap_or("-"),
                release.date.as_deref().unwrap_or("-"),
                release.country.as_deref().unwrap_or("-"),
                release.status.as_deref().unwrap_or("-"),
                release.barcode.is_some(),
                release.quality.as_deref().unwrap_or("-"),
                release.media_count,
                release.media_formats.join(", "),
                release
                    .media_track_counts
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                release.label_count,
                release.release_group_id.as_deref().unwrap_or("-"),
                release.release_group_title.as_deref().unwrap_or("-"),
                release
                    .release_group_first_release_date
                    .as_deref()
                    .unwrap_or("-")
            ));
        }

        Ok(releases)
    }
}

#[async_trait]
impl MusicBrainzDiscReleaseLookup for FilesystemMusicBrainzDiscReleaseLookup {
    async fn lookup_musicbrainz_disc_releases(
        &self,
        request: MusicBrainzDiscLookupRequest,
    ) -> Result<MusicBrainzDiscLookupResult> {
        if !self.enabled {
            return Ok(MusicBrainzDiscLookupResult::Disabled {
                diagnostic: "MusicBrainz lookup: disabled\n".into(),
            });
        }

        let mut diagnostic = String::from("MusicBrainz lookup: enabled\n");
        let toc = match build_musicbrainz_toc(&request.cue_paths) {
            Ok(toc) => toc,
            Err(err) => {
                diagnostic.push_str(&format!("MusicBrainz lookup: skipped: {err}\n"));
                return Ok(MusicBrainzDiscLookupResult::NotFound { diagnostic });
            }
        };
        diagnostic.push_str(&toc.diagnostic);

        let releases = match self.lookup_releases(&toc, &mut diagnostic).await {
            Ok(releases) => releases,
            Err(err) => {
                diagnostic.push_str(&format!("MusicBrainz lookup failed: {err}\n"));
                return Ok(MusicBrainzDiscLookupResult::NotFound { diagnostic });
            }
        };

        if releases.is_empty() {
            diagnostic.push_str("MusicBrainz lookup: no releases\n");
            Ok(MusicBrainzDiscLookupResult::NotFound { diagnostic })
        } else {
            diagnostic.push_str(&format!(
                "MusicBrainz lookup: found {} release(s)\n",
                releases.len()
            ));
            Ok(MusicBrainzDiscLookupResult::Found {
                releases,
                diagnostic,
            })
        }
    }
}

#[derive(Debug)]
struct MusicBrainzToc {
    toc: String,
    disc_id: String,
    diagnostic: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AudioFileLength {
    path: PathBuf,
    samples: u64,
    sample_rate: u64,
    cd_frames: u64,
}

fn build_musicbrainz_toc(cue_paths: &[PathBuf]) -> Result<MusicBrainzToc> {
    let mut diagnostic = String::new();
    if cue_paths.is_empty() {
        return Err(anyhow!("no CUE paths were provided"));
    }

    let mut track_offsets = Vec::new();
    let mut current_file_start = 0_u64;
    let mut audio_lengths = Vec::new();
    for cue_path in cue_paths {
        diagnostic.push_str(&format!(
            "MusicBrainz TOC CUE path: {}\n",
            cue_path.display()
        ));
        let cue = parse_from_file(&cue_path.to_string_lossy(), false)
            .map_err(|err| anyhow!("failed to parse CUE {}: {err}", cue_path.display()))?;
        let cue_dir = cue_path.parent().unwrap_or_else(|| Path::new("."));

        for cue_file in cue.files {
            let audio_path = cue_dir.join(&cue_file.file);
            let audio = read_audio_file_length(&audio_path)?;
            diagnostic.push_str(&format!(
                "MusicBrainz TOC audio: path={} samples={} sample_rate={} cd_frames={}\n",
                audio.path.display(),
                audio.samples,
                audio.sample_rate,
                audio.cd_frames
            ));

            for track in cue_file.tracks {
                if !track.format.eq_ignore_ascii_case("AUDIO") {
                    diagnostic.push_str(&format!(
                        "MusicBrainz TOC skipped non-audio track: number={} format={}\n",
                        track.no, track.format
                    ));
                    continue;
                }
                let Some(index_offset) = track_index_01_frames(&track) else {
                    diagnostic.push_str(&format!(
                        "MusicBrainz TOC skipped track without INDEX 01: number={}\n",
                        track.no
                    ));
                    continue;
                };
                track_offsets.push(current_file_start + index_offset + CD_LEAD_IN_FRAMES);
            }

            current_file_start += audio.cd_frames;
            audio_lengths.push(audio);
        }
    }

    if track_offsets.is_empty() {
        return Err(anyhow!("no audio tracks with INDEX 01 were found"));
    }
    if track_offsets.len() > 99 {
        return Err(anyhow!(
            "MusicBrainz TOC has too many tracks: {}",
            track_offsets.len()
        ));
    }

    let first_track = 1_u64;
    let last_track = track_offsets.len() as u64;
    let leadout = current_file_start + CD_LEAD_IN_FRAMES;
    let toc = std::iter::once(first_track.to_string())
        .chain(std::iter::once(last_track.to_string()))
        .chain(std::iter::once(leadout.to_string()))
        .chain(track_offsets.iter().map(u64::to_string))
        .collect::<Vec<_>>()
        .join(" ");
    let disc_id = musicbrainz_disc_id(first_track, last_track, leadout, &track_offsets);

    diagnostic.push_str(&format!(
        "MusicBrainz TOC track offsets: [{}]\n",
        track_offsets
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    ));
    diagnostic.push_str(&format!("MusicBrainz TOC leadout: {leadout}\n"));
    diagnostic.push_str(&format!("MusicBrainz TOC string: {toc}\n"));
    diagnostic.push_str(&format!("MusicBrainz Disc ID: {disc_id}\n"));

    Ok(MusicBrainzToc {
        toc,
        disc_id,
        diagnostic,
    })
}

fn track_index_01_frames(track: &rcue::cue::Track) -> Option<u64> {
    track
        .indices
        .iter()
        .find(|(index, _)| index == "01")
        .map(|(_, duration)| duration_to_cd_frames(*duration))
}

fn duration_to_cd_frames(duration: Duration) -> u64 {
    duration.as_secs() * CD_FRAMES_PER_SECOND
        + ((u64::from(duration.subsec_nanos()) * CD_FRAMES_PER_SECOND + 500_000_000)
            / 1_000_000_000)
}

fn read_audio_file_length(path: &Path) -> Result<AudioFileLength> {
    if !path.exists() {
        return Err(anyhow!(
            "referenced audio file is missing: {}",
            path.display()
        ));
    }
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("flac") => read_flac_file_length(path),
        Some("wav") | Some("wave") => read_wav_file_length(path),
        Some(extension) => Err(anyhow!(
            "unsupported referenced audio extension .{} for {}",
            extension,
            path.display()
        )),
        None => Err(anyhow!(
            "referenced audio file has no extension: {}",
            path.display()
        )),
    }
}

fn read_flac_file_length(path: &Path) -> Result<AudioFileLength> {
    let output = Command::new("metaflac")
        .arg("--show-total-samples")
        .arg("--show-sample-rate")
        .arg(path)
        .output()
        .map_err(|err| anyhow!("failed to run metaflac for {}: {err}", path.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "metaflac failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let values = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.parse::<u64>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| {
            anyhow!(
                "metaflac output was not numeric for {}: {err}",
                path.display()
            )
        })?;
    if values.len() != 2 {
        return Err(anyhow!(
            "metaflac returned {} value(s) for {}, expected total samples and sample rate",
            values.len(),
            path.display()
        ));
    }
    audio_length(path, values[0], values[1])
}

fn read_wav_file_length(path: &Path) -> Result<AudioFileLength> {
    let mut file =
        File::open(path).map_err(|err| anyhow!("failed to open WAV {}: {err}", path.display()))?;
    let mut riff = [0_u8; 12];
    file.read_exact(&mut riff)
        .map_err(|err| anyhow!("failed to read WAV header {}: {err}", path.display()))?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return Err(anyhow!("unsupported WAV layout for {}", path.display()));
    }

    let mut format_tag = None;
    let mut sample_rate = None;
    let mut block_align = None;
    let mut data_size = None;

    loop {
        let mut header = [0_u8; 8];
        match file.read_exact(&mut header) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(err) => {
                return Err(anyhow!(
                    "failed to read WAV chunk {}: {err}",
                    path.display()
                ))
            }
        }
        let chunk_id = &header[0..4];
        let chunk_size = u32::from_le_bytes(header[4..8].try_into().expect("slice length")) as u64;
        let chunk_start = file
            .stream_position()
            .map_err(|err| anyhow!("failed to inspect WAV {}: {err}", path.display()))?;

        if chunk_id == b"fmt " {
            let mut fmt = vec![0_u8; chunk_size as usize];
            file.read_exact(&mut fmt)
                .map_err(|err| anyhow!("failed to read WAV fmt chunk {}: {err}", path.display()))?;
            if fmt.len() < 16 {
                return Err(anyhow!("unsupported WAV fmt chunk for {}", path.display()));
            }
            format_tag = Some(u16::from_le_bytes([fmt[0], fmt[1]]));
            sample_rate = Some(u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]) as u64);
            block_align = Some(u16::from_le_bytes([fmt[12], fmt[13]]) as u64);
        } else if chunk_id == b"data" {
            data_size = Some(chunk_size);
            file.seek(SeekFrom::Current(chunk_size as i64))
                .map_err(|err| anyhow!("failed to skip WAV data {}: {err}", path.display()))?;
        } else {
            file.seek(SeekFrom::Current(chunk_size as i64))
                .map_err(|err| anyhow!("failed to skip WAV chunk {}: {err}", path.display()))?;
        }

        if chunk_size % 2 == 1 {
            file.seek(SeekFrom::Start(chunk_start + chunk_size + 1))
                .map_err(|err| anyhow!("failed to skip WAV padding {}: {err}", path.display()))?;
        }
    }

    match format_tag {
        Some(1 | 0xfffe) => {}
        Some(tag) => {
            return Err(anyhow!(
                "unsupported WAV format tag {} for {}",
                tag,
                path.display()
            ));
        }
        None => return Err(anyhow!("WAV fmt chunk is missing for {}", path.display())),
    }
    let sample_rate =
        sample_rate.ok_or_else(|| anyhow!("WAV sample rate is missing for {}", path.display()))?;
    let block_align =
        block_align.ok_or_else(|| anyhow!("WAV block align is missing for {}", path.display()))?;
    if block_align == 0 {
        return Err(anyhow!("WAV block align is zero for {}", path.display()));
    }
    let data_size =
        data_size.ok_or_else(|| anyhow!("WAV data chunk is missing for {}", path.display()))?;
    audio_length(path, data_size / block_align, sample_rate)
}

fn audio_length(path: &Path, samples: u64, sample_rate: u64) -> Result<AudioFileLength> {
    if sample_rate != CD_SAMPLE_RATE {
        return Err(anyhow!(
            "unsupported sample rate {} for {}, expected {}",
            sample_rate,
            path.display(),
            CD_SAMPLE_RATE
        ));
    }
    Ok(AudioFileLength {
        path: path.to_path_buf(),
        samples,
        sample_rate,
        cd_frames: samples * CD_FRAMES_PER_SECOND / sample_rate,
    })
}

fn musicbrainz_disc_id(first_track: u64, last_track: u64, leadout: u64, offsets: &[u64]) -> String {
    let mut input = format!("{first_track:02X}{last_track:02X}{leadout:08X}");
    for index in 0..99 {
        let offset = offsets.get(index).copied().unwrap_or(0);
        input.push_str(&format!("{offset:08X}"));
    }
    let digest = Sha1::digest(input.as_bytes());
    base64::engine::general_purpose::STANDARD
        .encode(digest)
        .replace('+', ".")
        .replace('/', "_")
        .replace('=', "-")
}

#[derive(Debug, Default)]
struct MusicBrainzRateLimiter {
    last_request: Mutex<Option<Instant>>,
}

impl MusicBrainzRateLimiter {
    async fn until_ready(&self) {
        let delay = {
            let mut last_request = self
                .last_request
                .lock()
                .expect("rate limiter mutex poisoned");
            let now = Instant::now();
            let delay = last_request
                .and_then(|last| {
                    Duration::from_secs(1).checked_sub(now.saturating_duration_since(last))
                })
                .unwrap_or_default();
            *last_request = Some(now + delay);
            delay
        };
        if !delay.is_zero() {
            sleep(delay).await;
        }
    }
}

fn global_rate_limiter() -> Arc<MusicBrainzRateLimiter> {
    static LIMITER: OnceLock<Arc<MusicBrainzRateLimiter>> = OnceLock::new();
    Arc::clone(LIMITER.get_or_init(|| Arc::new(MusicBrainzRateLimiter::default())))
}

#[derive(Debug, Deserialize)]
struct MusicBrainzDiscIdResponse {
    #[serde(default)]
    releases: Vec<MusicBrainzReleaseResponse>,
}

#[derive(Debug, Deserialize)]
struct MusicBrainzReleaseResponse {
    id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    barcode: Option<String>,
    #[serde(default)]
    quality: Option<String>,
    #[serde(default)]
    media: Vec<MusicBrainzMediumResponse>,
    #[serde(default, rename = "label-info")]
    label_info: Vec<serde_json::Value>,
    #[serde(default, rename = "release-group")]
    release_group: Option<MusicBrainzReleaseGroupResponse>,
}

#[derive(Debug, Deserialize)]
struct MusicBrainzMediumResponse {
    #[serde(default)]
    format: Option<String>,
    #[serde(default, rename = "track-count")]
    track_count: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct MusicBrainzReleaseGroupResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default, rename = "first-release-date")]
    first_release_date: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn builds_single_file_toc_and_disc_id() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("album.wav");
        write_wav(&wav, 44_100 * 4);
        let cue = tmp.path().join("album.cue");
        fs::write(
            &cue,
            r#"FILE "album.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 00:02:00
"#,
        )
        .unwrap();

        let toc = build_musicbrainz_toc(&[cue]).unwrap();

        assert_eq!(toc.toc, "1 2 450 150 300");
        assert_eq!(toc.disc_id, musicbrainz_disc_id(1, 2, 450, &[150, 300]));
        assert!(toc.diagnostic.contains("samples=176400"));
        assert!(toc.diagnostic.contains("track offsets: [150, 300]"));
    }

    #[test]
    fn builds_multi_file_toc_with_cumulative_offsets() {
        let tmp = tempfile::tempdir().unwrap();
        write_wav(&tmp.path().join("one.wav"), 44_100 * 2);
        write_wav(&tmp.path().join("two.wav"), 44_100 * 3);
        let cue = tmp.path().join("album.cue");
        fs::write(
            &cue,
            r#"FILE "one.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
FILE "two.wav" WAVE
  TRACK 02 AUDIO
    INDEX 01 00:00:00
  TRACK 03 AUDIO
    INDEX 01 00:01:00
"#,
        )
        .unwrap();

        let toc = build_musicbrainz_toc(&[cue]).unwrap();

        assert_eq!(toc.toc, "1 3 525 150 300 375");
    }

    #[test]
    fn wav_header_parsing_counts_samples() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("album.wav");
        write_wav(&wav, 1234);

        let length = read_wav_file_length(&wav).unwrap();

        assert_eq!(length.samples, 1234);
        assert_eq!(length.sample_rate, 44_100);
    }

    #[test]
    fn flac_length_uses_metaflac_output() {
        let _guard = ENV_LOCK.lock().unwrap();
        let original_path = std::env::var_os("PATH");
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let metaflac = bin.join("metaflac");
        fs::write(&metaflac, "#!/bin/sh\nprintf '88200\\n44100\\n'\n").unwrap();
        fs::set_permissions(&metaflac, fs::Permissions::from_mode(0o755)).unwrap();
        let flac = tmp.path().join("album.flac");
        fs::write(&flac, "").unwrap();
        std::env::set_var("PATH", &bin);

        let length = read_flac_file_length(&flac).unwrap();

        if let Some(path) = original_path {
            std::env::set_var("PATH", path);
        } else {
            std::env::remove_var("PATH");
        }
        assert_eq!(length.samples, 88_200);
        assert_eq!(length.sample_rate, 44_100);
        assert_eq!(length.cd_frames, 150);
    }

    #[test]
    fn unsupported_sample_rate_skips_toc() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("album.wav");
        write_wav_with_sample_rate(&wav, 48_000, 48_000);
        let cue = tmp.path().join("album.cue");
        fs::write(
            &cue,
            r#"FILE "album.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let err = build_musicbrainz_toc(&[cue]).unwrap_err().to_string();

        assert!(err.contains("unsupported sample rate 48000"));
    }

    #[test]
    fn skips_missing_index_and_non_audio_tracks_with_diagnostics() {
        let tmp = tempfile::tempdir().unwrap();
        write_wav(&tmp.path().join("album.wav"), 44_100);
        let cue = tmp.path().join("album.cue");
        fs::write(
            &cue,
            r#"FILE "album.wav" WAVE
  TRACK 01 MODE1/2352
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 00 00:00:00
"#,
        )
        .unwrap();

        let err = build_musicbrainz_toc(&[cue]).unwrap_err().to_string();

        assert!(err.contains("no audio tracks"));
    }

    #[tokio::test]
    async fn adapter_parses_musicbrainz_releases_and_sends_user_agent() {
        let tmp = tempfile::tempdir().unwrap();
        write_wav(&tmp.path().join("album.wav"), 44_100);
        let cue = tmp.path().join("album.cue");
        fs::write(
            &cue,
            r#"FILE "album.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();
        let body = r#"{"releases":[{"id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Album","date":"1984-01-01","country":"DE","status":"Official","barcode":"1234567890123","quality":"normal","media":[{"format":"CD","track-count":10},{"format":"Digital Media","track-count":10}],"label-info":[{"label":{"name":"Label"}}],"release-group":{"id":"11111111-1111-1111-1111-111111111111","title":"Album","first-release-date":"1984"}}]}"#;
        let (base_url, request) = serve_once(body).await;
        let lookup = FilesystemMusicBrainzDiscReleaseLookup::new(&MusicBrainzSettings {
            disc_lookup_enabled: true,
            base_url,
            trust_disc_lookup: false,
            add_missing_release_group_enabled: false,
        })
        .with_rate_limiter(Arc::new(MusicBrainzRateLimiter::default()));

        let result = lookup
            .lookup_musicbrainz_disc_releases(MusicBrainzDiscLookupRequest {
                cue_paths: vec![cue],
            })
            .await
            .unwrap();

        let MusicBrainzDiscLookupResult::Found {
            releases,
            diagnostic,
        } = result
        else {
            panic!("expected MusicBrainz releases");
        };
        assert_eq!(releases[0].id, "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
        assert_eq!(releases[0].media_count, 2);
        assert_eq!(releases[0].date.as_deref(), Some("1984-01-01"));
        assert_eq!(releases[0].country.as_deref(), Some("DE"));
        assert_eq!(releases[0].status.as_deref(), Some("Official"));
        assert_eq!(releases[0].barcode.as_deref(), Some("1234567890123"));
        assert_eq!(releases[0].quality.as_deref(), Some("normal"));
        assert_eq!(releases[0].media_formats, vec!["CD", "Digital Media"]);
        assert_eq!(releases[0].media_track_counts, vec![10, 10]);
        assert_eq!(releases[0].label_count, 1);
        assert_eq!(
            releases[0].release_group_id.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(releases[0].release_group_title.as_deref(), Some("Album"));
        assert_eq!(
            releases[0].release_group_first_release_date.as_deref(),
            Some("1984")
        );
        assert!(diagnostic.contains("barcode_present=true"));
        assert!(diagnostic.contains("release_group_id=11111111-1111-1111-1111-111111111111"));
        assert!(diagnostic.contains("formats=[CD, Digital Media]"));
        assert!(diagnostic.contains("fmt=json"));
        let request = request.lock().unwrap();
        assert!(request.contains("GET /ws/2/discid/"));
        assert!(request.contains("toc="));
        assert!(request.contains("fmt=json"));
        assert!(request.contains("inc=artists%2Brecordings%2Brelease-groups"));
        assert!(request
            .to_ascii_lowercase()
            .contains("user-agent: splittarr/"));
    }

    #[tokio::test]
    async fn adapter_treats_no_releases_as_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        write_wav(&tmp.path().join("album.wav"), 44_100);
        let cue = tmp.path().join("album.cue");
        fs::write(
            &cue,
            r#"FILE "album.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();
        let (base_url, _) = serve_once(r#"{"releases":[]}"#).await;
        let lookup = FilesystemMusicBrainzDiscReleaseLookup::new(&MusicBrainzSettings {
            disc_lookup_enabled: true,
            base_url,
            trust_disc_lookup: false,
            add_missing_release_group_enabled: false,
        })
        .with_rate_limiter(Arc::new(MusicBrainzRateLimiter::default()));

        let result = lookup
            .lookup_musicbrainz_disc_releases(MusicBrainzDiscLookupRequest {
                cue_paths: vec![cue],
            })
            .await
            .unwrap();

        let MusicBrainzDiscLookupResult::NotFound { diagnostic } = result else {
            panic!("expected no releases");
        };
        assert!(diagnostic.contains("MusicBrainz lookup: no releases"));
    }

    fn write_wav(path: &Path, samples: u64) {
        write_wav_with_sample_rate(path, samples, 44_100);
    }

    fn write_wav_with_sample_rate(path: &Path, samples: u64, sample_rate: u32) {
        let data_size = samples * 4;
        let riff_size = 36 + data_size;
        let byte_rate = sample_rate * 4;
        let mut file = File::create(path).unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(riff_size as u32).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16_u32.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&2_u16.to_le_bytes()).unwrap();
        file.write_all(&sample_rate.to_le_bytes()).unwrap();
        file.write_all(&byte_rate.to_le_bytes()).unwrap();
        file.write_all(&4_u16.to_le_bytes()).unwrap();
        file.write_all(&16_u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&(data_size as u32).to_le_bytes()).unwrap();
        file.write_all(&vec![0_u8; data_size as usize]).unwrap();
    }

    async fn serve_once(body: &'static str) -> (String, Arc<Mutex<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let request = Arc::new(Mutex::new(String::new()));
        let shared_request = Arc::clone(&request);
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0_u8; 4096];
            let bytes_read = socket.read(&mut buffer).await.unwrap();
            *shared_request.lock().unwrap() =
                String::from_utf8_lossy(&buffer[..bytes_read]).into_owned();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        (format!("http://{addr}"), request)
    }
}
