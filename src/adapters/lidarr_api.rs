use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::application::ports::{
    CueMetadataHint, DiscReleaseLookup, DiscReleaseLookupRequest, DiscReleaseLookupResult,
    ManualImportRequest, ManualImportResult, ManualImportTrigger, MusicBrainzDiscLookupRequest,
    MusicBrainzDiscLookupResult, MusicBrainzDiscRelease, MusicBrainzDiscReleaseLookup,
    NoopDiscReleaseLookup, NoopMusicBrainzDiscReleaseLookup, QueueSource,
};
use crate::bootstrap::settings::LidarrSettings;
use crate::domain::{FailedImportCandidate, QueueSnapshot};

const DEFAULT_MUSICBRAINZ_ADD_ALBUM_REFETCH_ATTEMPTS: usize = 5;
const DEFAULT_MUSICBRAINZ_ADD_ALBUM_REFETCH_DELAY: Duration = Duration::from_secs(1);
const DEFAULT_LIDARR_COMMAND_POLL_ATTEMPTS: usize = 60;
const DEFAULT_LIDARR_COMMAND_POLL_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct LidarrQueueSource {
    base_url: String,
    api_key: String,
    page_size: usize,
    max_pages: usize,
    manual_import_enabled: bool,
    client: reqwest::Client,
    disc_release_lookup: Arc<dyn DiscReleaseLookup>,
    musicbrainz_disc_release_lookup: Arc<dyn MusicBrainzDiscReleaseLookup>,
    trust_musicbrainz_disc_lookup: bool,
    add_missing_musicbrainz_release_group: bool,
    musicbrainz_add_album_refetch_attempts: usize,
    musicbrainz_add_album_refetch_delay: Duration,
    lidarr_command_poll_attempts: usize,
    lidarr_command_poll_delay: Duration,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueResponse {
    #[serde(default)]
    total_records: Option<usize>,
    #[serde(default)]
    records: Vec<QueueRecord>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueRecord {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    tracked_download_state: Option<String>,
    #[serde(default)]
    download_id: Option<String>,
    #[serde(default)]
    output_path: Option<String>,
}

impl LidarrQueueSource {
    pub fn new(settings: &LidarrSettings) -> Self {
        Self {
            base_url: settings.url.trim_end_matches('/').to_owned(),
            api_key: settings.api_key.clone(),
            page_size: settings.queue_page_size.max(1),
            max_pages: settings.queue_max_pages.max(1),
            manual_import_enabled: settings.manual_import_enabled,
            client: reqwest::Client::new(),
            disc_release_lookup: Arc::new(NoopDiscReleaseLookup),
            musicbrainz_disc_release_lookup: Arc::new(NoopMusicBrainzDiscReleaseLookup),
            trust_musicbrainz_disc_lookup: false,
            add_missing_musicbrainz_release_group: false,
            musicbrainz_add_album_refetch_attempts: DEFAULT_MUSICBRAINZ_ADD_ALBUM_REFETCH_ATTEMPTS,
            musicbrainz_add_album_refetch_delay: DEFAULT_MUSICBRAINZ_ADD_ALBUM_REFETCH_DELAY,
            lidarr_command_poll_attempts: DEFAULT_LIDARR_COMMAND_POLL_ATTEMPTS,
            lidarr_command_poll_delay: DEFAULT_LIDARR_COMMAND_POLL_DELAY,
        }
    }

    pub fn with_disc_release_lookup(mut self, lookup: Arc<dyn DiscReleaseLookup>) -> Self {
        self.disc_release_lookup = lookup;
        self
    }

    pub fn with_musicbrainz_disc_release_lookup(
        mut self,
        lookup: Arc<dyn MusicBrainzDiscReleaseLookup>,
    ) -> Self {
        self.musicbrainz_disc_release_lookup = lookup;
        self
    }

    pub fn with_musicbrainz_trust_disc_lookup(mut self, enabled: bool) -> Self {
        self.trust_musicbrainz_disc_lookup = enabled;
        self
    }

    pub fn with_musicbrainz_add_missing_release_group(mut self, enabled: bool) -> Self {
        self.add_missing_musicbrainz_release_group = enabled;
        self
    }

    #[cfg(test)]
    fn with_musicbrainz_add_album_refetch(mut self, attempts: usize, delay: Duration) -> Self {
        self.musicbrainz_add_album_refetch_attempts = attempts.max(1);
        self.musicbrainz_add_album_refetch_delay = delay;
        self
    }

    #[cfg(test)]
    fn with_lidarr_command_poll(mut self, attempts: usize, delay: Duration) -> Self {
        self.lidarr_command_poll_attempts = attempts.max(1);
        self.lidarr_command_poll_delay = delay;
        self
    }
}

impl QueueSource for LidarrQueueSource {
    async fn queue_snapshot(&self) -> Result<QueueSnapshot> {
        let mut page = 1_usize;
        let mut pages_fetched = 0_usize;
        let mut all_records = Vec::new();
        let mut expected_total_records = None;

        loop {
            if page > self.max_pages {
                return Err(anyhow!(
                    "lidarr queue pagination exceeded max pages ({}) while fetching page {}",
                    self.max_pages,
                    page
                ));
            }

            let response = self
                .client
                .get(format!("{}/api/v1/queue", self.base_url))
                .query(&[("page", page), ("pageSize", self.page_size)])
                .header("x-api-key", &self.api_key)
                .send()
                .await
                .map_err(|err| anyhow!("failed requesting lidarr queue page {page}: {err}"))?;
            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|err| anyhow!("failed reading lidarr queue page {page}: {err}"))?;

            if !status.is_success() {
                return Err(anyhow!(
                    "lidarr returned HTTP {status} for queue page {page}: {body}"
                ));
            }

            let queue: QueueResponse = serde_json::from_str(&body).map_err(|err| {
                anyhow!("lidarr returned invalid queue JSON for page {page}: {err}; body: {body}")
            })?;

            pages_fetched += 1;
            expected_total_records = expected_total_records.or(queue.total_records);
            let current_page_count = queue.records.len();
            all_records.extend(queue.records);

            if current_page_count == 0 {
                break;
            }
            if let Some(total) = expected_total_records {
                if all_records.len() >= total {
                    break;
                }
            }
            if current_page_count < self.page_size {
                break;
            }

            page += 1;
        }

        let active_download_ids = all_records
            .iter()
            .filter_map(QueueRecord::download_id)
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        let failed_imports = all_records
            .iter()
            .filter_map(QueueRecord::as_candidate)
            .collect::<Vec<_>>();

        Ok(QueueSnapshot {
            total_records: all_records.len(),
            pages_fetched,
            active_download_ids,
            failed_imports,
        })
    }
}

impl ManualImportTrigger for LidarrQueueSource {
    async fn trigger_manual_import(
        &self,
        request: ManualImportRequest,
    ) -> Result<ManualImportResult> {
        if !self.manual_import_enabled {
            return Ok(ManualImportResult::Disabled);
        }

        let response = self
            .client
            .get(format!("{}/api/v1/manualimport", self.base_url))
            .query(&[
                ("folder", request.download.output_path.as_str()),
                ("downloadId", request.download.download_id.as_str()),
                ("filterExistingFiles", "true"),
                ("replaceExistingFiles", "true"),
            ])
            .header("x-api-key", &self.api_key)
            .send()
            .await
            .map_err(|err| anyhow!("failed requesting lidarr manual import candidates: {err}"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| anyhow!("failed reading lidarr manual import candidates: {err}"))?;

        if !status.is_success() {
            return Err(anyhow!(
                "lidarr returned HTTP {status} for manual import candidates: {body}"
            ));
        }

        let candidates: Vec<ManualImportResource> = serde_json::from_str(&body).map_err(|err| {
            anyhow!("lidarr returned invalid manual import JSON: {err}; body: {body}")
        })?;
        let selection = self
            .select_manual_import_files(&request, &candidates)
            .await?;
        let (files, diagnostic) = match selection {
            ManualImportSelection::Selected { files, diagnostic } => (files, diagnostic),
            ManualImportSelection::Skipped { reason, diagnostic } => {
                return Ok(ManualImportResult::Skipped { reason, diagnostic });
            }
        };
        let imported_track_count = files.len();
        let command = ManualImportCommand {
            name: "ManualImport",
            import_mode: "Move",
            replace_existing_files: true,
            files,
        };
        let command_body = serde_json::to_string(&command)
            .map_err(|err| anyhow!("failed serializing lidarr manual import command: {err}"))?;
        let mut diagnostic = diagnostic;
        diagnostic.push_str(&format!(
            "Lidarr manual import command: posting /api/v1/command files={imported_track_count}\n"
        ));
        append_lidarr_manual_import_command_files(&mut diagnostic, &command.files);
        let response = self
            .client
            .post(format!("{}/api/v1/command", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("content-type", "application/json")
            .body(command_body)
            .send()
            .await
            .map_err(|err| anyhow!("failed starting lidarr manual import command: {err}"))?;
        let status = response.status();
        let body = response.text().await.map_err(|err| {
            anyhow!("failed reading lidarr manual import command response: {err}")
        })?;

        if !status.is_success() {
            return Err(anyhow!(
                "lidarr returned HTTP {status} for manual import command: {body}\n{diagnostic}"
            ));
        }
        let command_response: LidarrCommandResource = serde_json::from_str(&body).map_err(|err| {
            anyhow!("lidarr returned invalid manual import command JSON: {err}; body: {body}\n{diagnostic}")
        })?;
        append_lidarr_command_diagnostic(
            &mut diagnostic,
            "Lidarr manual import command accepted",
            &command_response,
        );
        let wait_outcome = self
            .await_lidarr_command(command_response, &mut diagnostic)
            .await?;
        if wait_outcome == CommandWaitOutcome::Completed {
            verify_manual_import_source_files_moved(&command.files, &mut diagnostic).await?;
        }

        Ok(ManualImportResult::Started {
            imported_track_count,
            diagnostic,
        })
    }
}

impl LidarrQueueSource {
    async fn await_lidarr_command(
        &self,
        mut command: LidarrCommandResource,
        diagnostic: &mut String,
    ) -> Result<CommandWaitOutcome> {
        let Some(command_id) = command.positive_id() else {
            diagnostic.push_str(
                "Lidarr manual import command polling: skipped because response had no command id\n",
            );
            return Ok(CommandWaitOutcome::NotCompleted);
        };
        if command.status.is_none() {
            diagnostic.push_str(
                "Lidarr manual import command polling: skipped because response had no status\n",
            );
            return Ok(CommandWaitOutcome::NotCompleted);
        }

        for attempt in 1..=self.lidarr_command_poll_attempts {
            match lidarr_command_outcome(&command) {
                CommandOutcome::Successful => {
                    diagnostic.push_str(&format!(
                        "Lidarr manual import command decision: completed successfully command_id={command_id} attempt={attempt}\n"
                    ));
                    return Ok(CommandWaitOutcome::Completed);
                }
                CommandOutcome::Failed(reason) => {
                    diagnostic.push_str(&format!(
                        "Lidarr manual import command decision: failed command_id={command_id} attempt={attempt}: {reason}\n"
                    ));
                    return Err(anyhow!(
                        "lidarr manual import command failed: {reason}\n{diagnostic}"
                    ));
                }
                CommandOutcome::Running => {}
            }

            if attempt == self.lidarr_command_poll_attempts {
                diagnostic.push_str(&format!(
                    "Lidarr manual import command decision: still running command_id={command_id} attempts={attempt}\n"
                ));
                return Ok(CommandWaitOutcome::NotCompleted);
            }

            if attempt > 1 || !self.lidarr_command_poll_delay.is_zero() {
                tokio::time::sleep(self.lidarr_command_poll_delay).await;
            }
            command = self
                .fetch_lidarr_command(command_id, diagnostic, attempt)
                .await?;
        }

        Ok(CommandWaitOutcome::NotCompleted)
    }

    async fn fetch_lidarr_command(
        &self,
        command_id: i64,
        diagnostic: &mut String,
        attempt: usize,
    ) -> Result<LidarrCommandResource> {
        let response = self
            .client
            .get(format!("{}/api/v1/command/{command_id}", self.base_url))
            .header("x-api-key", &self.api_key)
            .send()
            .await
            .map_err(|err| {
                anyhow!("failed requesting lidarr manual import command status {command_id}: {err}")
            })?;
        let status = response.status();
        let body = response.text().await.map_err(|err| {
            anyhow!("failed reading lidarr manual import command status {command_id}: {err}")
        })?;
        if !status.is_success() {
            return Err(anyhow!(
                "lidarr returned HTTP {status} for manual import command status {command_id}: {body}\n{diagnostic}"
            ));
        }
        let command = serde_json::from_str(&body).map_err(|err| {
            anyhow!(
                "lidarr returned invalid manual import command status JSON for {command_id}: {err}; body: {body}\n{diagnostic}"
            )
        })?;
        append_lidarr_command_diagnostic(
            diagnostic,
            &format!("Lidarr manual import command poll attempt={attempt}"),
            &command,
        );
        Ok(command)
    }

    async fn select_manual_import_files(
        &self,
        request: &ManualImportRequest,
        candidates: &[ManualImportResource],
    ) -> Result<ManualImportSelection> {
        match select_manual_import_files(request, candidates) {
            ManualImportSelection::Selected { files, diagnostic } => {
                Ok(ManualImportSelection::Selected { files, diagnostic })
            }
            ManualImportSelection::Skipped { reason, diagnostic } => {
                self.select_fallback_manual_import_files(request, candidates, reason, diagnostic)
                    .await
            }
        }
    }

    async fn select_fallback_manual_import_files(
        &self,
        request: &ManualImportRequest,
        candidates: &[ManualImportResource],
        direct_reason: String,
        mut diagnostic: String,
    ) -> Result<ManualImportSelection> {
        let fallback = match fallback_prerequisites(request, candidates) {
            Ok(fallback) => fallback,
            Err(reason) => {
                diagnostic.push_str("Fallback manual import: skipped\n");
                diagnostic.push_str(&format!("Fallback reason: {reason}\n"));
                return Ok(ManualImportSelection::Skipped {
                    reason: direct_reason,
                    diagnostic,
                });
            }
        };

        diagnostic.push_str("Fallback manual import: evaluating Lidarr album data\n");
        diagnostic.push_str(&format!("Fallback artist id: {}\n", fallback.artist_id));
        let hints = AlbumMatchHints::from_request(request);
        append_album_match_hints(&mut diagnostic, &hints);

        let albums = self.fetch_artist_albums(fallback.artist_id).await?;
        let mut musicbrainz_add_attempted = false;
        let album_match = match self
            .select_fallback_album(
                &fallback.artist,
                &albums,
                &hints,
                &mut diagnostic,
                &mut musicbrainz_add_attempted,
            )
            .await
        {
            Ok(album_match) => album_match,
            Err(reason) => {
                if !musicbrainz_add_attempted {
                    if let Some(album_match) = self
                        .try_add_missing_musicbrainz_release_group(
                            &fallback.artist,
                            lidarr_source_artist(&albums, fallback.artist_id),
                            &hints,
                            &mut diagnostic,
                            &mut musicbrainz_add_attempted,
                        )
                        .await
                        .map_err(|err| anyhow!(err))?
                    {
                        return self
                            .manual_import_selection_for_album_match(
                                request,
                                &fallback,
                                album_match,
                                diagnostic,
                            )
                            .await;
                    }
                }
                diagnostic.push_str(&format!("Fallback decision: skipped: {reason}\n"));
                return Ok(ManualImportSelection::Skipped { reason, diagnostic });
            }
        };

        self.manual_import_selection_for_album_match(request, &fallback, album_match, diagnostic)
            .await
    }

    async fn manual_import_selection_for_album_match(
        &self,
        request: &ManualImportRequest,
        fallback: &FallbackPrerequisites<'_>,
        album_match: SelectedFallbackAlbum,
        mut diagnostic: String,
    ) -> Result<ManualImportSelection> {
        let tracks = self.fetch_album_tracks(album_match.album_id).await?;
        let mapped_tracks =
            match map_generated_tracks_to_lidarr_tracks(request, &tracks, &mut diagnostic) {
                Ok(mapped_tracks) => mapped_tracks,
                Err(reason) => {
                    diagnostic.push_str(&format!("Fallback decision: skipped: {reason}\n"));
                    return Ok(ManualImportSelection::Skipped { reason, diagnostic });
                }
            };

        let mut files = Vec::with_capacity(fallback.candidates_by_path.len());
        for generated_track in &request.generated_tracks {
            let path = generated_track.to_string_lossy().to_string();
            let Some(candidate) = fallback.candidates_by_path.get(&path) else {
                let reason = format!("{} no longer has a fallback candidate", path);
                diagnostic.push_str(&format!("Fallback decision: skipped: {reason}\n"));
                return Ok(ManualImportSelection::Skipped { reason, diagnostic });
            };
            let Some(quality) = candidate.quality.clone() else {
                let reason = format!("{} is missing quality", candidate.path);
                diagnostic.push_str(&format!("Fallback decision: skipped: {reason}\n"));
                return Ok(ManualImportSelection::Skipped { reason, diagnostic });
            };
            let Some(track_id) = mapped_tracks.get(&path).copied() else {
                let reason = format!("{} was not mapped to a Lidarr track", path);
                diagnostic.push_str(&format!("Fallback decision: skipped: {reason}\n"));
                return Ok(ManualImportSelection::Skipped { reason, diagnostic });
            };
            files.push(ManualImportFile {
                path: candidate.path.clone(),
                artist_id: fallback.artist_id,
                album_id: album_match.album_id,
                album_release_id: album_match.album_release_id,
                track_ids: vec![track_id],
                quality,
                indexer_flags: candidate.indexer_flags,
                download_id: request.download.download_id.clone(),
                disable_release_switching: false,
            });
        }

        diagnostic.push_str(&format!(
            "Fallback decision: selected album_id={} album_release_id={} for {} track(s)\n",
            album_match.album_id,
            album_match.album_release_id,
            files.len()
        ));
        Ok(ManualImportSelection::Selected { files, diagnostic })
    }

    async fn fetch_artist_albums(&self, artist_id: i64) -> Result<Vec<LidarrAlbumResource>> {
        let response = self
            .client
            .get(format!("{}/api/v1/album", self.base_url))
            .query(&[("artistId", artist_id)])
            .header("x-api-key", &self.api_key)
            .send()
            .await
            .map_err(|err| {
                anyhow!("failed requesting lidarr albums for artist {artist_id}: {err}")
            })?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| anyhow!("failed reading lidarr albums for artist {artist_id}: {err}"))?;
        if !status.is_success() {
            return Err(anyhow!(
                "lidarr returned HTTP {status} for artist albums: {body}"
            ));
        }
        serde_json::from_str(&body)
            .map_err(|err| anyhow!("lidarr returned invalid album JSON: {err}; body: {body}"))
    }

    async fn fetch_album_tracks(&self, album_id: i64) -> Result<Vec<LidarrTrackResource>> {
        let response = self
            .client
            .get(format!("{}/api/v1/track", self.base_url))
            .query(&[("albumId", album_id)])
            .header("x-api-key", &self.api_key)
            .send()
            .await
            .map_err(|err| {
                anyhow!("failed requesting lidarr tracks for album {album_id}: {err}")
            })?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| anyhow!("failed reading lidarr tracks for album {album_id}: {err}"))?;
        if !status.is_success() {
            return Err(anyhow!(
                "lidarr returned HTTP {status} for album tracks: {body}"
            ));
        }
        serde_json::from_str(&body)
            .map_err(|err| anyhow!("lidarr returned invalid track JSON: {err}; body: {body}"))
    }

    async fn select_fallback_album(
        &self,
        artist: &ManualImportArtist,
        albums: &[LidarrAlbumResource],
        hints: &AlbumMatchHints,
        diagnostic: &mut String,
        musicbrainz_add_attempted: &mut bool,
    ) -> std::result::Result<SelectedFallbackAlbum, String> {
        let candidate = match select_fallback_album_candidate(albums, hints, diagnostic) {
            Ok(candidate) => candidate,
            Err(reason)
                if self.trust_musicbrainz_disc_lookup
                    && reason == "multiple Lidarr albums matched fallback hints" =>
            {
                diagnostic.push_str(
                    "Fallback album match was ambiguous; trying trusted MusicBrainz widening\n",
                );
                if let Some(album_match) = self
                    .select_trusted_album_with_musicbrainz_lookup(albums, hints, diagnostic)
                    .await?
                {
                    return Ok(SelectedFallbackAlbum {
                        album_id: album_match.album.id,
                        album_release_id: album_match.release.id,
                    });
                }
                return Err(reason);
            }
            Err(reason) => return Err(reason),
        };
        if candidate.releases.len() > 1 {
            if let Some(album_match) = self
                .select_album_with_musicbrainz_lookup(
                    albums,
                    &candidate,
                    artist,
                    hints,
                    diagnostic,
                    musicbrainz_add_attempted,
                )
                .await?
            {
                return Ok(album_match);
            }
        }

        if !candidate.requires_disc_lookup {
            if let Some(release) = select_fallback_release(candidate.releases.clone()) {
                diagnostic.push_str(&format!(
                    "Fallback selected album: album_id={} title={} release_id={}\n",
                    candidate.album.id, candidate.album.title, release.id
                ));
                return Ok(SelectedFallbackAlbum {
                    album_id: candidate.album.id,
                    album_release_id: release.id,
                });
            }
        }

        let release = self
            .select_release_with_disc_lookup(candidate.releases, hints, diagnostic)
            .await?
            .ok_or_else(|| "multiple Lidarr album releases matched fallback hints".to_owned())?;
        diagnostic.push_str(&format!(
            "Fallback selected album using disc lookup: album_id={} title={} release_id={} foreign_release_id={}\n",
            candidate.album.id,
            candidate.album.title,
            release.id,
            release.foreign_release_id.as_deref().unwrap_or("-")
        ));
        Ok(SelectedFallbackAlbum {
            album_id: candidate.album.id,
            album_release_id: release.id,
        })
    }

    async fn select_album_with_musicbrainz_lookup<'a>(
        &self,
        albums: &'a [LidarrAlbumResource],
        primary: &FallbackAlbumCandidate<'a>,
        artist: &ManualImportArtist,
        hints: &AlbumMatchHints,
        diagnostic: &mut String,
        musicbrainz_add_attempted: &mut bool,
    ) -> std::result::Result<Option<SelectedFallbackAlbum>, String> {
        diagnostic.push_str("Fallback MusicBrainz release lookup: evaluating\n");
        diagnostic.push_str(&format!(
            "Fallback MusicBrainz trusted widening: {}\n",
            self.trust_musicbrainz_disc_lookup
        ));
        diagnostic.push_str("Fallback Lidarr releases for MusicBrainz/GnuDB:\n");
        for release in &primary.releases {
            diagnostic.push_str(&format!(
                "  - release_id={} foreign_release_id={} title={} track_count={} duration={} format={} country=[{}] label=[{}] monitored={}\n",
                release.id,
                release.foreign_release_id.as_deref().unwrap_or("-"),
                release.title.as_deref().unwrap_or("-"),
                release.track_count,
                release.duration,
                release.format.as_deref().unwrap_or("-"),
                release.country.join(", "),
                release.label.join(", "),
                release.monitored
            ));
        }

        let Some(musicbrainz_releases) =
            self.lookup_musicbrainz_releases(hints, diagnostic).await?
        else {
            return Ok(None);
        };

        if let Some(release) = select_musicbrainz_release_for_releases(
            &primary.releases,
            hints,
            &musicbrainz_releases,
            diagnostic,
        )? {
            diagnostic.push_str(&format!(
                "Fallback selected album using MusicBrainz: album_id={} title={} release_id={} foreign_release_id={}\n",
                primary.album.id,
                primary.album.title,
                release.id,
                release.foreign_release_id.as_deref().unwrap_or("-")
            ));
            return Ok(Some(SelectedFallbackAlbum {
                album_id: primary.album.id,
                album_release_id: release.id,
            }));
        }

        if self.trust_musicbrainz_disc_lookup {
            if let Some(album_match) = select_trusted_musicbrainz_album_release(
                albums,
                hints,
                &musicbrainz_releases,
                diagnostic,
            ) {
                return Ok(Some(SelectedFallbackAlbum {
                    album_id: album_match.album.id,
                    album_release_id: album_match.release.id,
                }));
            }
        }

        self.try_add_missing_musicbrainz_release_group_from_releases(
            artist,
            primary.album.artist.as_ref(),
            hints,
            &musicbrainz_releases,
            diagnostic,
            musicbrainz_add_attempted,
        )
        .await
    }

    async fn select_trusted_album_with_musicbrainz_lookup<'a>(
        &self,
        albums: &'a [LidarrAlbumResource],
        hints: &AlbumMatchHints,
        diagnostic: &mut String,
    ) -> std::result::Result<Option<FallbackAlbumMatch<'a>>, String> {
        diagnostic.push_str("Fallback MusicBrainz release lookup: evaluating\n");
        diagnostic.push_str(&format!(
            "Fallback MusicBrainz trusted widening: {}\n",
            self.trust_musicbrainz_disc_lookup
        ));
        let Some(musicbrainz_releases) =
            self.lookup_musicbrainz_releases(hints, diagnostic).await?
        else {
            return Ok(None);
        };
        Ok(select_trusted_musicbrainz_album_release(
            albums,
            hints,
            &musicbrainz_releases,
            diagnostic,
        ))
    }

    async fn try_add_missing_musicbrainz_release_group(
        &self,
        artist: &ManualImportArtist,
        source_artist: Option<&Value>,
        hints: &AlbumMatchHints,
        diagnostic: &mut String,
        musicbrainz_add_attempted: &mut bool,
    ) -> std::result::Result<Option<SelectedFallbackAlbum>, String> {
        diagnostic.push_str(&format!(
            "Fallback MusicBrainz add missing release group: {}\n",
            self.add_missing_musicbrainz_release_group
        ));
        if !self.add_missing_musicbrainz_release_group {
            diagnostic.push_str(
                "MusicBrainz add missing release group decision: disabled, falling through\n",
            );
            return Ok(None);
        }

        let Some(musicbrainz_releases) =
            self.lookup_musicbrainz_releases(hints, diagnostic).await?
        else {
            diagnostic.push_str(
                "MusicBrainz add missing release group decision: no MusicBrainz releases, falling through\n",
            );
            return Ok(None);
        };

        self.try_add_missing_musicbrainz_release_group_from_releases(
            artist,
            source_artist,
            hints,
            &musicbrainz_releases,
            diagnostic,
            musicbrainz_add_attempted,
        )
        .await
    }

    async fn try_add_missing_musicbrainz_release_group_from_releases(
        &self,
        artist: &ManualImportArtist,
        source_artist: Option<&Value>,
        hints: &AlbumMatchHints,
        musicbrainz_releases: &[MusicBrainzDiscRelease],
        diagnostic: &mut String,
        musicbrainz_add_attempted: &mut bool,
    ) -> std::result::Result<Option<SelectedFallbackAlbum>, String> {
        diagnostic.push_str(&format!(
            "Fallback MusicBrainz add missing release group: {}\n",
            self.add_missing_musicbrainz_release_group
        ));
        if !self.add_missing_musicbrainz_release_group {
            diagnostic.push_str(
                "MusicBrainz add missing release group decision: disabled, falling through\n",
            );
            return Ok(None);
        }
        *musicbrainz_add_attempted = true;

        let release_group_id =
            match single_musicbrainz_release_group_id(musicbrainz_releases, diagnostic) {
                Some(release_group_id) => release_group_id,
                None => return Ok(None),
            };

        let Some(mut album) = self
            .search_lidarr_album_by_release_group(&release_group_id, diagnostic)
            .await
        else {
            return Ok(None);
        };
        let search_release_count = album.releases.len();
        let artist_payload = source_artist
            .cloned()
            .or_else(|| album.artist.clone())
            .or_else(|| serde_json::to_value(artist).ok());
        album.artist_id = artist.id;
        album.artist = artist_payload;
        album.releases.clear();
        album.monitored = Some(false);
        album.add_options = Some(LidarrAddAlbumOptions {
            add_type: "manual".to_owned(),
            search_for_new_album: false,
        });
        diagnostic.push_str(&format!(
            "MusicBrainz add missing release group: prepared Lidarr add album payload artist_source={} stripped_search_releases={}\n",
            if source_artist.is_some() {
                "lidarr-album"
            } else if album.artist.is_some() {
                "search-album-or-manual-import"
            } else {
                "none"
            },
            search_release_count
        ));

        let Some(added_album) = self.add_lidarr_album(album, diagnostic).await else {
            return Ok(None);
        };

        let added_album_id = (added_album.id > 0).then_some(added_album.id);
        let added_foreign_album_id = added_album
            .foreign_album_id
            .as_deref()
            .unwrap_or(&release_group_id);
        let Some(album) = self
            .fetch_added_musicbrainz_album(
                artist.id,
                added_album_id,
                added_foreign_album_id,
                hints.track_count,
                diagnostic,
            )
            .await
        else {
            return Ok(None);
        };

        let releases = matching_releases(&album, hints.track_count);
        diagnostic.push_str(&format!(
            "MusicBrainz added Lidarr album candidate: album_id={} title={} release_group_id={} matching_releases={}\n",
            album.id,
            album.title,
            album.foreign_album_id.as_deref().unwrap_or("-"),
            releases.len()
        ));
        if releases.is_empty() {
            diagnostic.push_str(
                "MusicBrainz add missing release group decision: added album has no compatible release, falling through\n",
            );
            return Ok(None);
        }

        let Some(release) = select_musicbrainz_release_for_releases(
            &releases,
            hints,
            musicbrainz_releases,
            diagnostic,
        )?
        else {
            diagnostic.push_str(
                "MusicBrainz add missing release group decision: no compatible added Lidarr release, falling through\n",
            );
            return Ok(None);
        };

        diagnostic.push_str(&format!(
            "MusicBrainz add missing release group decision: added release-group selected album_id={} album_title={} release_id={} foreign_release_id={}\n",
            album.id,
            album.title,
            release.id,
            release.foreign_release_id.as_deref().unwrap_or("-")
        ));
        Ok(Some(SelectedFallbackAlbum {
            album_id: album.id,
            album_release_id: release.id,
        }))
    }

    async fn fetch_added_musicbrainz_album(
        &self,
        artist_id: i64,
        added_album_id: Option<i64>,
        added_foreign_album_id: &str,
        track_count: usize,
        diagnostic: &mut String,
    ) -> Option<LidarrAlbumResource> {
        for attempt in 1..=self.musicbrainz_add_album_refetch_attempts {
            let albums = match self.fetch_artist_albums(artist_id).await {
                Ok(albums) => albums,
                Err(err) => {
                    diagnostic.push_str(&format!(
                        "MusicBrainz add missing release group decision: failed refetching Lidarr albums after add attempt={attempt}: {err}\n",
                    ));
                    return None;
                }
            };
            let album = albums.into_iter().find(|album| {
                added_album_id.is_some_and(|id| album.id == id)
                    || album
                        .foreign_album_id
                        .as_deref()
                        .is_some_and(|id| id.eq_ignore_ascii_case(added_foreign_album_id))
            });
            let Some(album) = album else {
                diagnostic.push_str(&format!(
                    "MusicBrainz add missing release group refetch attempt={attempt}: added album not present foreign_album_id={added_foreign_album_id}\n",
                ));
                if attempt < self.musicbrainz_add_album_refetch_attempts {
                    tokio::time::sleep(self.musicbrainz_add_album_refetch_delay).await;
                    continue;
                }
                diagnostic.push_str(&format!(
                    "MusicBrainz add missing release group decision: added album was not present after refetch foreign_album_id={added_foreign_album_id}\n",
                ));
                return None;
            };

            let matching_releases = matching_releases(&album, track_count);
            diagnostic.push_str(&format!(
                "MusicBrainz add missing release group refetch attempt={attempt}: album_id={} title={} releases={} matching_releases={}\n",
                album.id,
                album.title,
                album.releases.len(),
                matching_releases.len()
            ));
            if !matching_releases.is_empty()
                || attempt == self.musicbrainz_add_album_refetch_attempts
            {
                return Some(album);
            }

            tokio::time::sleep(self.musicbrainz_add_album_refetch_delay).await;
        }

        None
    }

    async fn search_lidarr_album_by_release_group(
        &self,
        release_group_id: &str,
        diagnostic: &mut String,
    ) -> Option<LidarrAlbumResource> {
        let term = format!("lidarr:{release_group_id}");
        diagnostic.push_str(&format!(
            "MusicBrainz add missing release group search: path=/api/v1/search query=term={term}\n"
        ));
        let response = match self
            .client
            .get(format!("{}/api/v1/search", self.base_url))
            .query(&[("term", term.as_str())])
            .header("x-api-key", &self.api_key)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                diagnostic.push_str(&format!(
                    "MusicBrainz add missing release group decision: failed requesting Lidarr search: {err}\n",
                ));
                return None;
            }
        };
        let status = response.status();
        let body = match response.text().await {
            Ok(body) => body,
            Err(err) => {
                diagnostic.push_str(&format!(
                    "MusicBrainz add missing release group decision: failed reading Lidarr search: {err}\n",
                ));
                return None;
            }
        };
        if !status.is_success() {
            diagnostic.push_str(&format!(
                "MusicBrainz add missing release group decision: Lidarr search returned HTTP {status}: {body}\n",
            ));
            return None;
        }

        let results: Vec<LidarrSearchResource> = match serde_json::from_str(&body) {
            Ok(results) => results,
            Err(err) => {
                diagnostic.push_str(&format!(
                    "MusicBrainz add missing release group decision: invalid Lidarr search JSON: {err}; body: {body}\n",
                ));
                return None;
            }
        };
        diagnostic.push_str(&format!(
            "MusicBrainz add missing release group search result count: {}\n",
            results.len()
        ));
        let matching = results
            .into_iter()
            .filter_map(|result| result.album)
            .inspect(|album| {
                diagnostic.push_str(&format!(
                    "  - search album title={} foreign_album_id={} id={}\n",
                    album.title,
                    album.foreign_album_id.as_deref().unwrap_or("-"),
                    album.id
                ));
            })
            .filter(|album| {
                album
                    .foreign_album_id
                    .as_deref()
                    .is_some_and(|id| id.eq_ignore_ascii_case(release_group_id))
            })
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            diagnostic.push_str(&format!(
                "MusicBrainz add missing release group decision: expected one matching Lidarr search album, got {}\n",
                matching.len()
            ));
            return None;
        }
        diagnostic.push_str(
            "MusicBrainz add missing release group decision: Lidarr search album accepted\n",
        );
        matching.into_iter().next()
    }

    async fn add_lidarr_album(
        &self,
        album: LidarrAlbumResource,
        diagnostic: &mut String,
    ) -> Option<LidarrAlbumResource> {
        diagnostic.push_str("MusicBrainz add missing release group: POST /api/v1/album monitored=false addType=manual searchForNewAlbum=false\n");
        let body = match serde_json::to_string(&album) {
            Ok(body) => body,
            Err(err) => {
                diagnostic.push_str(&format!(
                    "MusicBrainz add missing release group decision: failed serializing Lidarr album add request: {err}\n",
                ));
                return None;
            }
        };
        let response = match self
            .client
            .post(format!("{}/api/v1/album", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                diagnostic.push_str(&format!(
                    "MusicBrainz add missing release group decision: failed posting Lidarr album: {err}\n",
                ));
                return None;
            }
        };
        let status = response.status();
        let body = match response.text().await {
            Ok(body) => body,
            Err(err) => {
                diagnostic.push_str(&format!(
                    "MusicBrainz add missing release group decision: failed reading Lidarr add album response: {err}\n",
                ));
                return None;
            }
        };
        if !status.is_success() {
            diagnostic.push_str(&format!(
                "MusicBrainz add missing release group decision: Lidarr add album returned HTTP {status}: {body}\n",
            ));
            return None;
        }
        let album: LidarrAlbumResource = match serde_json::from_str(&body) {
            Ok(album) => album,
            Err(err) => {
                diagnostic.push_str(&format!(
                    "MusicBrainz add missing release group decision: invalid Lidarr add album JSON: {err}; body: {body}\n",
                ));
                return None;
            }
        };
        diagnostic.push_str(&format!(
            "MusicBrainz add missing release group: added album_id={} title={} foreign_album_id={}\n",
            album.id,
            album.title,
            album.foreign_album_id.as_deref().unwrap_or("-")
        ));
        Some(album)
    }

    async fn select_release_with_disc_lookup<'a>(
        &self,
        releases: Vec<&'a LidarrAlbumRelease>,
        hints: &AlbumMatchHints,
        diagnostic: &mut String,
    ) -> std::result::Result<Option<&'a LidarrAlbumRelease>, String> {
        diagnostic.push_str("Fallback GnuDB release lookup: evaluating\n");
        diagnostic.push_str("Fallback Lidarr releases for GnuDB:\n");
        for release in &releases {
            diagnostic.push_str(&format!(
                "  - release_id={} foreign_release_id={} title={} track_count={} duration={} format={} country=[{}] label=[{}] monitored={}\n",
                release.id,
                release.foreign_release_id.as_deref().unwrap_or("-"),
                release.title.as_deref().unwrap_or("-"),
                release.track_count,
                release.duration,
                release.format.as_deref().unwrap_or("-"),
                release.country.join(", "),
                release.label.join(", "),
                release.monitored
            ));
        }

        let Some(disc_id) = hints.disc_id.as_ref().filter(|disc_id| !disc_id.is_empty()) else {
            diagnostic
                .push_str("Fallback GnuDB release lookup: skipped because CUE DISCID is missing\n");
            return Ok(None);
        };

        let result = self
            .disc_release_lookup
            .lookup_disc_release(DiscReleaseLookupRequest {
                disc_id: disc_id.clone(),
                artist: hints.artist.clone(),
                album_title: hints.album_title.clone(),
                year: hints.year,
                track_count: hints.track_count,
                track_titles_by_number: hints
                    .track_titles_by_number
                    .iter()
                    .map(|(number, title)| (*number, title.clone()))
                    .collect(),
            })
            .await
            .map_err(|err| format!("GnuDB lookup failed: {err}"))?;

        match result {
            DiscReleaseLookupResult::Disabled {
                diagnostic: lookup_diagnostic,
            }
            | DiscReleaseLookupResult::NotFound {
                diagnostic: lookup_diagnostic,
            } => {
                diagnostic.push_str(&lookup_diagnostic);
                Ok(None)
            }
            DiscReleaseLookupResult::Found {
                candidates,
                diagnostic: lookup_diagnostic,
            } => {
                diagnostic.push_str(&lookup_diagnostic);
                let release_by_foreign_id = releases
                    .iter()
                    .filter_map(|release| {
                        Some((
                            release.foreign_release_id.as_deref()?.to_ascii_lowercase(),
                            *release,
                        ))
                    })
                    .collect::<HashMap<_, _>>();
                let mut matches = Vec::new();
                for candidate in candidates {
                    let matching_art_ids = candidate
                        .art_ids
                        .iter()
                        .filter_map(|art_id| {
                            release_by_foreign_id
                                .get(&art_id.to_ascii_lowercase())
                                .copied()
                                .map(|release| (art_id.clone(), release))
                        })
                        .collect::<Vec<_>>();
                    diagnostic.push_str(&format!(
                        "GnuDB candidate intersection: category={} id={} art_ids=[{}] matches={}\n",
                        candidate.category,
                        candidate.entry_id,
                        candidate.art_ids.join(", "),
                        matching_art_ids.len()
                    ));
                    if matching_art_ids.len() == 1 {
                        matches.push(matching_art_ids[0].clone());
                    }
                }

                if matches.len() == 1 {
                    diagnostic.push_str(&format!(
                        "GnuDB selected foreign_release_id={} release_id={}\n",
                        matches[0].0, matches[0].1.id
                    ));
                    Ok(Some(matches[0].1))
                } else {
                    diagnostic.push_str(&format!(
                        "GnuDB release lookup did not produce exactly one Lidarr release match: {}\n",
                        matches.len()
                    ));
                    Ok(None)
                }
            }
        }
    }

    async fn lookup_musicbrainz_releases(
        &self,
        hints: &AlbumMatchHints,
        diagnostic: &mut String,
    ) -> std::result::Result<Option<Vec<MusicBrainzDiscRelease>>, String> {
        let cue_paths = hints.cue_paths.clone();
        if cue_paths.is_empty() {
            diagnostic.push_str(
                "Fallback MusicBrainz release lookup: skipped because no CUE paths are available\n",
            );
            return Ok(None);
        }

        let result = self
            .musicbrainz_disc_release_lookup
            .lookup_musicbrainz_disc_releases(MusicBrainzDiscLookupRequest { cue_paths })
            .await
            .map_err(|err| format!("MusicBrainz lookup failed: {err}"))?;

        match result {
            MusicBrainzDiscLookupResult::Disabled {
                diagnostic: lookup_diagnostic,
            }
            | MusicBrainzDiscLookupResult::NotFound {
                diagnostic: lookup_diagnostic,
            } => {
                diagnostic.push_str(&lookup_diagnostic);
                diagnostic.push_str("Fallback MusicBrainz decision: fell through to GnuDB\n");
                Ok(None)
            }
            MusicBrainzDiscLookupResult::Found {
                releases: musicbrainz_releases,
                diagnostic: lookup_diagnostic,
            } => {
                diagnostic.push_str(&lookup_diagnostic);
                Ok(Some(musicbrainz_releases))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct MusicBrainzReleaseScore {
    exact_mbid: bool,
    monitored: bool,
    track_count_compatible: bool,
    format_preference: i32,
    metadata_completeness: i32,
}

struct RankedMusicBrainzRelease<'a> {
    release: &'a LidarrAlbumRelease,
    foreign_release_id: String,
    score: MusicBrainzReleaseScore,
}

struct RankedTrustedMusicBrainzRelease<'a> {
    album: &'a LidarrAlbumResource,
    ranked: RankedMusicBrainzRelease<'a>,
}

fn single_musicbrainz_release_group_id(
    musicbrainz_releases: &[MusicBrainzDiscRelease],
    diagnostic: &mut String,
) -> Option<String> {
    let mut ids = Vec::new();
    let mut missing = 0_usize;
    for release in musicbrainz_releases {
        let Some(id) = release.release_group_id.as_deref() else {
            missing += 1;
            continue;
        };
        let normalized = id.to_ascii_lowercase();
        if !ids.contains(&normalized) {
            ids.push(normalized);
        }
    }

    diagnostic.push_str(&format!(
        "MusicBrainz add missing release group distinct release_group_ids=[{}] missing_release_group_ids={}\n",
        ids.join(", "),
        missing
    ));
    if missing > 0 {
        diagnostic.push_str(
            "MusicBrainz add missing release group decision: skipped because at least one MusicBrainz release lacks a release-group ID\n",
        );
        return None;
    }
    if ids.len() != 1 {
        diagnostic.push_str(&format!(
            "MusicBrainz add missing release group decision: expected one distinct release-group ID, got {}\n",
            ids.len()
        ));
        return None;
    }
    ids.into_iter().next()
}

fn select_musicbrainz_release_for_releases<'a>(
    releases: &[&'a LidarrAlbumRelease],
    hints: &AlbumMatchHints,
    musicbrainz_releases: &[MusicBrainzDiscRelease],
    diagnostic: &mut String,
) -> std::result::Result<Option<&'a LidarrAlbumRelease>, String> {
    let mut releases_by_foreign_id: HashMap<String, Vec<&LidarrAlbumRelease>> = HashMap::new();
    for release in releases {
        if let Some(foreign_release_id) = release.foreign_release_id.as_deref() {
            releases_by_foreign_id
                .entry(foreign_release_id.to_ascii_lowercase())
                .or_default()
                .push(*release);
        }
    }

    let mut matches = Vec::new();
    for musicbrainz_release in musicbrainz_releases {
        let id = musicbrainz_release.id.to_ascii_lowercase();
        let matched = releases_by_foreign_id
            .get(&id)
            .filter(|releases| releases.len() == 1)
            .map(|releases| releases[0]);
        let match_count = releases_by_foreign_id.get(&id).map_or(0, Vec::len);
        diagnostic.push_str(&format!(
            "MusicBrainz candidate intersection: release_id={} title={} media_count={} matches={}\n",
            musicbrainz_release.id,
            musicbrainz_release.title.as_deref().unwrap_or("-"),
            musicbrainz_release.media_count,
            match_count
        ));
        if let Some(release) = matched {
            matches.push((musicbrainz_release, release));
        }
    }

    if matches.len() == 1 {
        diagnostic.push_str(&format!(
            "MusicBrainz decision: exact single MBID selected foreign_release_id={} release_id={}\n",
            matches[0].0.id, matches[0].1.id
        ));
        Ok(Some(matches[0].1))
    } else {
        diagnostic.push_str(&format!(
            "MusicBrainz exact intersection count: {}\n",
            matches.len()
        ));
        select_ranked_musicbrainz_release(
            releases,
            hints,
            musicbrainz_releases,
            matches,
            diagnostic,
        )
    }
}

fn select_trusted_musicbrainz_album_release<'a>(
    albums: &'a [LidarrAlbumResource],
    hints: &AlbumMatchHints,
    musicbrainz_releases: &[MusicBrainzDiscRelease],
    diagnostic: &mut String,
) -> Option<FallbackAlbumMatch<'a>> {
    let trusted_titles = trusted_musicbrainz_titles(musicbrainz_releases);
    diagnostic.push_str(&format!(
        "MusicBrainz trusted titles: [{}]\n",
        trusted_titles.join(", ")
    ));
    let compatible_musicbrainz_metadata = musicbrainz_releases
        .iter()
        .filter(|release| release.media_track_counts.contains(&hints.track_count))
        .map(metadata_completeness)
        .max();
    let Some(default_metadata_completeness) = compatible_musicbrainz_metadata else {
        diagnostic.push_str(
            "MusicBrainz trusted decision: no MusicBrainz release has compatible track count\n",
        );
        return None;
    };

    let mut candidates = Vec::new();
    diagnostic.push_str("MusicBrainz trusted widened Lidarr album candidates:\n");
    for album in albums {
        let normalized_album = normalize_match_text(&album.title);
        let title_match = trusted_titles
            .iter()
            .any(|trusted_title| titles_strongly_match(trusted_title, &normalized_album));
        let release_year = album.release_date.as_deref().and_then(first_year);
        let year_match = hints
            .year
            .zip(release_year)
            .is_some_and(|(hint_year, album_year)| hint_year == album_year);
        let matching_releases = matching_releases(album, hints.track_count);
        diagnostic.push_str(&format!(
            "  - album_id={} title={} normalized={} release_date={} title_match={} year_match={} matching_releases={}\n",
            album.id,
            album.title,
            normalized_album,
            album.release_date.as_deref().unwrap_or("-"),
            title_match,
            year_match,
            matching_releases.len()
        ));
        for release in &matching_releases {
            diagnostic.push_str(&format!(
                "      release_id={} foreign_release_id={} title={} track_count={} monitored={} format={}\n",
                release.id,
                release.foreign_release_id.as_deref().unwrap_or("-"),
                release.title.as_deref().unwrap_or("-"),
                release.track_count,
                release.monitored,
                release.format.as_deref().unwrap_or("-")
            ));
        }
        if !title_match {
            continue;
        }

        for release in matching_releases {
            let Some(foreign_release_id) = release.foreign_release_id.as_deref() else {
                continue;
            };
            let matching_musicbrainz_release = musicbrainz_releases
                .iter()
                .find(|candidate| candidate.id.eq_ignore_ascii_case(foreign_release_id));
            let metadata_completeness = matching_musicbrainz_release
                .map(metadata_completeness)
                .unwrap_or(default_metadata_completeness);
            let ranked = ranked_musicbrainz_candidate(
                release,
                foreign_release_id,
                matching_musicbrainz_release.is_some(),
                metadata_completeness,
            )?;
            candidates.push(RankedTrustedMusicBrainzRelease { album, ranked });
        }
    }

    diagnostic.push_str(&format!(
        "MusicBrainz trusted candidate count: {}\n",
        candidates.len()
    ));
    for candidate in &candidates {
        diagnostic.push_str(&format!(
            "MusicBrainz trusted candidate album_id={} album_title={} ",
            candidate.album.id, candidate.album.title
        ));
        append_musicbrainz_score_diagnostic(diagnostic, &candidate.ranked);
    }

    let Some(best_index) = best_unique_trusted_musicbrainz_candidate_index(&candidates) else {
        if candidates.is_empty() {
            diagnostic.push_str(
                "MusicBrainz trusted decision: no compatible widened Lidarr release, falling through to GnuDB\n",
            );
        } else {
            diagnostic.push_str(
                "MusicBrainz trusted decision: ranked candidates tied, falling through to GnuDB\n",
            );
        }
        return None;
    };
    let best = &candidates[best_index];
    diagnostic.push_str(&format!(
        "MusicBrainz trusted decision: widened album/release selected album_id={} album_title={} foreign_release_id={} release_id={}\n",
        best.album.id,
        best.album.title,
        best.ranked.foreign_release_id,
        best.ranked.release.id
    ));
    Some(FallbackAlbumMatch {
        album: best.album,
        release: best.ranked.release,
    })
}

fn select_ranked_musicbrainz_release<'a>(
    lidarr_releases: &[&'a LidarrAlbumRelease],
    hints: &AlbumMatchHints,
    musicbrainz_releases: &[MusicBrainzDiscRelease],
    exact_matches: Vec<(&MusicBrainzDiscRelease, &'a LidarrAlbumRelease)>,
    diagnostic: &mut String,
) -> std::result::Result<Option<&'a LidarrAlbumRelease>, String> {
    let candidates = if exact_matches.is_empty() {
        ranked_best_effort_musicbrainz_candidates(lidarr_releases, hints, musicbrainz_releases)
    } else {
        exact_matches
            .into_iter()
            .filter_map(|(musicbrainz_release, lidarr_release)| {
                ranked_musicbrainz_candidate(
                    lidarr_release,
                    &musicbrainz_release.id,
                    true,
                    metadata_completeness(musicbrainz_release),
                )
            })
            .collect::<Vec<_>>()
    };

    diagnostic.push_str(&format!(
        "MusicBrainz best-effort candidate count: {}\n",
        candidates.len()
    ));
    for candidate in &candidates {
        append_musicbrainz_score_diagnostic(diagnostic, candidate);
    }

    let Some(best_index) = best_unique_musicbrainz_candidate_index(&candidates) else {
        if candidates.is_empty() {
            diagnostic.push_str(
                "MusicBrainz decision: no compatible Lidarr release, falling through to GnuDB\n",
            );
        } else {
            diagnostic.push_str(
                "MusicBrainz decision: ranked candidates tied, falling through to GnuDB\n",
            );
        }
        return Ok(None);
    };
    let best = &candidates[best_index];

    let decision = if best.score.exact_mbid {
        "ranked exact MBID selected"
    } else {
        "best-effort compatible release selected"
    };
    diagnostic.push_str(&format!(
        "MusicBrainz decision: {decision} foreign_release_id={} release_id={}\n",
        best.foreign_release_id, best.release.id
    ));
    Ok(Some(best.release))
}

fn ranked_best_effort_musicbrainz_candidates<'a>(
    lidarr_releases: &[&'a LidarrAlbumRelease],
    hints: &AlbumMatchHints,
    musicbrainz_releases: &[MusicBrainzDiscRelease],
) -> Vec<RankedMusicBrainzRelease<'a>> {
    let compatible_musicbrainz_metadata = musicbrainz_releases
        .iter()
        .filter(|release| release.media_track_counts.contains(&hints.track_count))
        .map(metadata_completeness)
        .max();
    let Some(metadata_completeness) = compatible_musicbrainz_metadata else {
        return Vec::new();
    };

    lidarr_releases
        .iter()
        .filter(|release| release.track_count == hints.track_count)
        .filter_map(|release| {
            ranked_musicbrainz_candidate(
                release,
                release.foreign_release_id.as_deref()?,
                false,
                metadata_completeness,
            )
        })
        .collect()
}

fn ranked_musicbrainz_candidate<'a>(
    release: &'a LidarrAlbumRelease,
    foreign_release_id: &str,
    exact_mbid: bool,
    metadata_completeness: i32,
) -> Option<RankedMusicBrainzRelease<'a>> {
    Some(RankedMusicBrainzRelease {
        release,
        foreign_release_id: foreign_release_id.to_owned(),
        score: MusicBrainzReleaseScore {
            exact_mbid,
            monitored: release.monitored,
            track_count_compatible: true,
            format_preference: release_format_preference(release.format.as_deref()),
            metadata_completeness,
        },
    })
}

fn best_unique_musicbrainz_candidate_index(
    candidates: &[RankedMusicBrainzRelease<'_>],
) -> Option<usize> {
    let (best_index, best) = candidates
        .iter()
        .enumerate()
        .max_by_key(|(_, candidate)| candidate.score)?;
    let tied_best_count = candidates
        .iter()
        .filter(|candidate| candidate.score == best.score)
        .count();
    (tied_best_count == 1).then_some(best_index)
}

fn best_unique_trusted_musicbrainz_candidate_index(
    candidates: &[RankedTrustedMusicBrainzRelease<'_>],
) -> Option<usize> {
    let (best_index, best) = candidates
        .iter()
        .enumerate()
        .max_by_key(|(_, candidate)| candidate.ranked.score)?;
    let tied_best_count = candidates
        .iter()
        .filter(|candidate| candidate.ranked.score == best.ranked.score)
        .count();
    (tied_best_count == 1).then_some(best_index)
}

fn trusted_musicbrainz_titles(musicbrainz_releases: &[MusicBrainzDiscRelease]) -> Vec<String> {
    let mut titles = Vec::new();
    for release in musicbrainz_releases {
        for title in [
            release.title.as_deref(),
            release.release_group_title.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            let normalized = normalize_match_text(title);
            if !normalized.is_empty() && !titles.contains(&normalized) {
                titles.push(normalized);
            }
        }
    }
    titles
}

fn append_musicbrainz_score_diagnostic(
    diagnostic: &mut String,
    candidate: &RankedMusicBrainzRelease<'_>,
) {
    diagnostic.push_str(&format!(
        "MusicBrainz ranked Lidarr candidate: release_id={} foreign_release_id={} exact_mbid={} monitored={} track_count_compatible={} format_preference={} metadata_completeness={} title={} track_count={} format={}\n",
        candidate.release.id,
        candidate.foreign_release_id,
        candidate.score.exact_mbid,
        candidate.score.monitored,
        candidate.score.track_count_compatible,
        candidate.score.format_preference,
        candidate.score.metadata_completeness,
        candidate.release.title.as_deref().unwrap_or("-"),
        candidate.release.track_count,
        candidate.release.format.as_deref().unwrap_or("-")
    ));
}

fn metadata_completeness(release: &MusicBrainzDiscRelease) -> i32 {
    i32::from(release.barcode.is_some())
        + i32::from(release.date.is_some())
        + i32::from(release.country.is_some())
        + i32::from(release.status.is_some())
        + i32::from(release.quality.is_some())
        + i32::from(release.label_count > 0)
        + i32::from(release.media_count > 0)
        + i32::from(!release.media_formats.is_empty())
        + i32::from(!release.media_track_counts.is_empty())
        + i32::from(release.release_group_id.is_some())
        + i32::from(release.release_group_title.is_some())
        + i32::from(release.release_group_first_release_date.is_some())
}

fn release_format_preference(format: Option<&str>) -> i32 {
    let Some(format) = format.map(|format| format.to_ascii_lowercase()) else {
        return 0;
    };
    if format.contains("cd")
        || format.contains("digital")
        || format.contains("download")
        || format.contains("web")
    {
        2
    } else if format.contains("vinyl")
        || format.contains("7\"")
        || format.contains("12\"")
        || format.contains("cassette")
    {
        0
    } else {
        1
    }
}

impl QueueRecord {
    fn download_id(&self) -> Option<&str> {
        self.download_id
            .as_deref()
            .filter(|value| !value.is_empty())
    }

    fn as_candidate(&self) -> Option<FailedImportCandidate> {
        let status = self.status.as_deref()?;
        let tracked_download_state = self.tracked_download_state.as_deref()?;
        if status != "completed" || tracked_download_state != "importFailed" {
            return None;
        }

        let download_id = self.download_id.as_ref()?.trim();
        let output_path = self.output_path.as_ref()?.trim();
        if download_id.is_empty() || output_path.is_empty() {
            return None;
        }

        Some(FailedImportCandidate {
            download_id: download_id.to_owned(),
            title: self
                .title
                .clone()
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| download_id.to_owned()),
            status: status.to_owned(),
            output_path: output_path.to_owned(),
            tracked_download_state: tracked_download_state.to_owned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManualImportResource {
    path: String,
    #[serde(default)]
    artist: Option<ManualImportArtist>,
    #[serde(default)]
    album: Option<ManualImportAlbum>,
    #[serde(default)]
    album_release_id: i64,
    #[serde(default)]
    tracks: Vec<ManualImportTrack>,
    #[serde(default)]
    quality: Option<Value>,
    #[serde(default)]
    indexer_flags: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManualImportArtist {
    id: i64,
    #[serde(default)]
    artist_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManualImportAlbum {
    id: i64,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManualImportTrack {
    id: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LidarrAlbumResource {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    artist_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    artist: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    foreign_album_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    release_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    monitored: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    releases: Vec<LidarrAlbumRelease>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    add_options: Option<LidarrAddAlbumOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LidarrAlbumRelease {
    id: i64,
    #[serde(default)]
    album_id: i64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    track_count: usize,
    #[serde(default)]
    monitored: bool,
    #[serde(default)]
    foreign_release_id: Option<String>,
    #[serde(default)]
    country: Vec<String>,
    #[serde(default)]
    label: Vec<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    duration: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LidarrAddAlbumOptions {
    add_type: String,
    search_for_new_album: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LidarrSearchResource {
    #[serde(default)]
    album: Option<LidarrAlbumResource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LidarrTrackResource {
    id: i64,
    #[serde(default)]
    album_id: i64,
    #[serde(default)]
    absolute_track_number: i64,
    #[serde(default)]
    track_number: Option<String>,
    #[serde(default)]
    title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManualImportCommand {
    name: &'static str,
    import_mode: &'static str,
    replace_existing_files: bool,
    files: Vec<ManualImportFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManualImportFile {
    path: String,
    artist_id: i64,
    album_id: i64,
    album_release_id: i64,
    track_ids: Vec<i64>,
    quality: Value,
    indexer_flags: i64,
    download_id: String,
    disable_release_switching: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LidarrCommandResource {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    command_name: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    exception: Option<String>,
}

impl LidarrCommandResource {
    fn positive_id(&self) -> Option<i64> {
        (self.id > 0).then_some(self.id)
    }
}

enum ManualImportSelection {
    Selected {
        files: Vec<ManualImportFile>,
        diagnostic: String,
    },
    Skipped {
        reason: String,
        diagnostic: String,
    },
}

fn select_manual_import_files(
    request: &ManualImportRequest,
    candidates: &[ManualImportResource],
) -> ManualImportSelection {
    let mut diagnostic = manual_import_diagnostic_header(request, candidates);
    let mut selected = Vec::with_capacity(request.generated_tracks.len());

    for generated_track in &request.generated_tracks {
        let matches = candidates
            .iter()
            .filter(|candidate| paths_match(&candidate.path, generated_track))
            .collect::<Vec<_>>();
        diagnostic.push_str(&format!(
            "Match check: {} matched {} candidate(s)\n",
            generated_track.display(),
            matches.len()
        ));
        if matches.len() != 1 {
            return skipped_selection(
                "each generated track must match exactly one Lidarr candidate",
                diagnostic,
            );
        }

        let file = match manual_import_file(&request.download.download_id, matches[0]) {
            Ok(file) => file,
            Err(reason) => return skipped_selection(reason, diagnostic),
        };
        selected.push(file);
    }

    if selected.is_empty() {
        return skipped_selection("no generated tracks were selected for import", diagnostic);
    }
    if !one_album_release(&selected) {
        diagnostic.push_str(
            "Album release check: selected files did not agree on one artist/album/release\n",
        );
        return skipped_selection(
            "selected tracks must resolve to one album release",
            diagnostic,
        );
    }
    if !cue_hints_match(request, candidates, &selected) {
        diagnostic.push_str(
            "Cue hint check: Lidarr candidate album did not match CUE album hint or track count\n",
        );
        return skipped_selection("CUE hints did not match Lidarr candidates", diagnostic);
    }

    diagnostic.push_str(&format!(
        "Decision: selected {} track(s) for Lidarr manual import\n",
        selected.len()
    ));
    ManualImportSelection::Selected {
        files: selected,
        diagnostic,
    }
}

fn skipped_selection(reason: impl Into<String>, mut diagnostic: String) -> ManualImportSelection {
    let reason = reason.into();
    diagnostic.push_str(&format!("Decision: skipped manual import: {reason}\n"));
    ManualImportSelection::Skipped { reason, diagnostic }
}

enum CommandOutcome {
    Running,
    Successful,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandWaitOutcome {
    Completed,
    NotCompleted,
}

fn lidarr_command_outcome(command: &LidarrCommandResource) -> CommandOutcome {
    if command
        .result
        .as_deref()
        .is_some_and(|result| result.eq_ignore_ascii_case("unsuccessful"))
    {
        return CommandOutcome::Failed(lidarr_command_failure_reason(command));
    }

    match command
        .status
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("completed") => CommandOutcome::Successful,
        Some("failed" | "aborted" | "cancelled" | "orphaned") => {
            CommandOutcome::Failed(lidarr_command_failure_reason(command))
        }
        _ => CommandOutcome::Running,
    }
}

fn lidarr_command_failure_reason(command: &LidarrCommandResource) -> String {
    command
        .exception
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            command
                .message
                .as_deref()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or("command did not report a failure reason")
        .to_owned()
}

fn append_lidarr_command_diagnostic(
    diagnostic: &mut String,
    label: &str,
    command: &LidarrCommandResource,
) {
    diagnostic.push_str(&format!(
        "{label}: id={} name={} command_name={} status={} result={} message={} exception={}\n",
        command.id,
        command.name.as_deref().unwrap_or("-"),
        command.command_name.as_deref().unwrap_or("-"),
        command.status.as_deref().unwrap_or("-"),
        command.result.as_deref().unwrap_or("-"),
        command.message.as_deref().unwrap_or("-"),
        command.exception.as_deref().unwrap_or("-")
    ));
}

fn append_lidarr_manual_import_command_files(diagnostic: &mut String, files: &[ManualImportFile]) {
    diagnostic.push_str("Lidarr manual import command files:\n");
    for file in files {
        diagnostic.push_str(&format!(
            "  - path={} artist_id={} album_id={} album_release_id={} track_ids={:?} quality={} indexer_flags={} download_id={} disable_release_switching={}\n",
            file.path,
            file.artist_id,
            file.album_id,
            file.album_release_id,
            file.track_ids,
            quality_summary(&file.quality),
            file.indexer_flags,
            file.download_id,
            file.disable_release_switching
        ));
    }
}

fn quality_summary(quality: &Value) -> String {
    let quality_value = quality.get("quality").unwrap_or(quality);
    let id = quality_value
        .get("id")
        .and_then(Value::as_i64)
        .map_or_else(|| "-".to_owned(), |id| id.to_string());
    let name = quality_value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("-");
    format!("id={id} name={name}")
}

async fn verify_manual_import_source_files_moved(
    files: &[ManualImportFile],
    diagnostic: &mut String,
) -> Result<()> {
    let submitted_paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let remaining_paths = tokio::task::spawn_blocking(move || {
        submitted_paths
            .into_iter()
            .filter(|path| Path::new(path).exists())
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|err| anyhow!("blocking source verification task failed to join: {err}"))?;

    diagnostic.push_str(&format!(
        "Lidarr manual import source verification: submitted={} remaining_at_source={}\n",
        files.len(),
        remaining_paths.len()
    ));
    for path in &remaining_paths {
        diagnostic.push_str(&format!("  - remaining source: {path}\n"));
    }

    if remaining_paths.is_empty() {
        diagnostic.push_str(
            "Lidarr manual import source verification: all submitted source files were moved or removed\n",
        );
        return Ok(());
    }

    Err(anyhow!(
        "lidarr manual import command reported success, but {} of {} submitted source files still exist at source\n{diagnostic}",
        remaining_paths.len(),
        files.len()
    ))
}

fn paths_match(candidate_path: &str, generated_track: &Path) -> bool {
    Path::new(candidate_path) == generated_track
}

fn manual_import_file(
    download_id: &str,
    item: &ManualImportResource,
) -> std::result::Result<ManualImportFile, String> {
    let artist_id = item
        .artist
        .as_ref()
        .map(|artist| artist.id)
        .ok_or_else(|| format!("{} is missing artist", item.path))?;
    let album_id = item
        .album
        .as_ref()
        .map(|album| album.id)
        .ok_or_else(|| format!("{} is missing album", item.path))?;
    if item.album_release_id <= 0 {
        return Err(format!("{} is missing album release", item.path));
    }
    let quality = item
        .quality
        .clone()
        .ok_or_else(|| format!("{} is missing quality", item.path))?;
    let track_ids = item.tracks.iter().map(|track| track.id).collect::<Vec<_>>();
    if track_ids.is_empty() {
        return Err(format!("{} is missing tracks", item.path));
    }

    Ok(ManualImportFile {
        path: item.path.clone(),
        artist_id,
        album_id,
        album_release_id: item.album_release_id,
        track_ids,
        quality,
        indexer_flags: item.indexer_flags,
        download_id: download_id.to_owned(),
        disable_release_switching: false,
    })
}

fn manual_import_diagnostic_header(
    request: &ManualImportRequest,
    candidates: &[ManualImportResource],
) -> String {
    let mut diagnostic = String::new();
    diagnostic.push_str("Manual import diagnostic\n");
    diagnostic.push_str("Generated split tracks:\n");
    for track in &request.generated_tracks {
        diagnostic.push_str(&format!("  - {}\n", track.display()));
    }
    diagnostic.push_str("CUE hints:\n");
    for hint in &request.cue_hints {
        diagnostic.push_str(&format!(
            "  - path={} album={} performer={} catalog={} track_count={}\n",
            hint.path.display(),
            hint.album_title.as_deref().unwrap_or("-"),
            hint.performer.as_deref().unwrap_or("-"),
            hint.catalog.as_deref().unwrap_or("-"),
            hint.track_count
        ));
        for (key, value) in &hint.comments {
            diagnostic.push_str(&format!("    REM {key} {value}\n"));
        }
    }
    diagnostic.push_str("Lidarr manual import candidates:\n");
    for candidate in candidates {
        let track_ids = candidate
            .tracks
            .iter()
            .map(|track| track.id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        diagnostic.push_str(&format!(
            "  - path={} artist_id={} artist={} album_id={} album={} album_release_id={} track_ids=[{}] quality_present={} indexer_flags={}\n",
            candidate.path,
            candidate
                .artist
                .as_ref()
                .map(|artist| artist.id.to_string())
                .unwrap_or_else(|| "-".into()),
            candidate
                .artist
                .as_ref()
                .and_then(|artist| artist.artist_name.as_deref())
                .unwrap_or("-"),
            candidate
                .album
                .as_ref()
                .map(|album| album.id.to_string())
                .unwrap_or_else(|| "-".into()),
            candidate
                .album
                .as_ref()
                .and_then(|album| album.title.as_deref())
                .unwrap_or("-"),
            candidate.album_release_id,
            track_ids,
            candidate.quality.is_some(),
            candidate.indexer_flags
        ));
    }
    diagnostic
}

fn one_album_release(files: &[ManualImportFile]) -> bool {
    let first = &files[0];
    files.iter().all(|file| {
        file.artist_id == first.artist_id
            && file.album_id == first.album_id
            && file.album_release_id == first.album_release_id
    })
}

fn cue_hints_match(
    request: &ManualImportRequest,
    candidates: &[ManualImportResource],
    selected: &[ManualImportFile],
) -> bool {
    let expected_track_count = request
        .cue_hints
        .iter()
        .map(|hint| hint.track_count)
        .sum::<usize>();
    if expected_track_count > 0 && expected_track_count != selected.len() {
        return false;
    }

    let album_titles = request
        .cue_hints
        .iter()
        .filter_map(|hint| hint.album_title.as_deref())
        .map(normalize_hint)
        .collect::<HashSet<_>>();
    if album_titles.len() == 1 {
        let selected_album_titles = candidates
            .iter()
            .filter(|candidate| {
                selected
                    .iter()
                    .any(|file| paths_match(&candidate.path, Path::new(&file.path)))
            })
            .filter_map(|candidate| candidate.album.as_ref()?.title.as_deref())
            .map(normalize_hint)
            .collect::<HashSet<_>>();
        if !selected_album_titles.is_empty()
            && selected_album_titles
                .iter()
                .all(|title| !album_titles.contains(title))
        {
            return false;
        }
    }

    true
}

fn normalize_hint(value: &str) -> String {
    value.trim().to_lowercase()
}

struct FallbackPrerequisites<'a> {
    artist_id: i64,
    artist: ManualImportArtist,
    candidates_by_path: HashMap<String, &'a ManualImportResource>,
}

fn fallback_prerequisites<'a>(
    request: &ManualImportRequest,
    candidates: &'a [ManualImportResource],
) -> std::result::Result<FallbackPrerequisites<'a>, String> {
    if request.generated_tracks.is_empty() {
        return Err("no generated tracks were selected for import".into());
    }

    let mut artist_id = None;
    let mut artist = None;
    let mut candidates_by_path = HashMap::new();
    let mut saw_missing_lidarr_metadata = false;

    for generated_track in &request.generated_tracks {
        let matches = candidates
            .iter()
            .filter(|candidate| paths_match(&candidate.path, generated_track))
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err("each generated track must match exactly one Lidarr candidate".into());
        }
        let candidate = matches[0];
        let candidate_artist_id = candidate
            .artist
            .as_ref()
            .map(|artist| artist.id)
            .ok_or_else(|| format!("{} is missing artist", candidate.path))?;
        let candidate_artist = candidate
            .artist
            .as_ref()
            .expect("candidate artist is present after candidate_artist_id");
        if candidate.quality.is_none() {
            return Err(format!("{} is missing quality", candidate.path));
        }
        if candidate.album.is_none()
            || candidate.album_release_id <= 0
            || candidate.tracks.is_empty()
        {
            saw_missing_lidarr_metadata = true;
        }
        match artist_id {
            Some(id) if id != candidate_artist_id => {
                return Err("fallback candidates must agree on one artist".into());
            }
            Some(_) => {}
            None => {
                artist_id = Some(candidate_artist_id);
                artist = Some(candidate_artist.clone());
            }
        }
        candidates_by_path.insert(generated_track.to_string_lossy().to_string(), candidate);
    }

    if !saw_missing_lidarr_metadata {
        return Err("direct Lidarr metadata was present; fallback is not applicable".into());
    }

    Ok(FallbackPrerequisites {
        artist_id: artist_id.expect("artist_id is set when generated tracks are non-empty"),
        artist: artist.expect("artist is set when generated tracks are non-empty"),
        candidates_by_path,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AlbumMatchHints {
    cue_paths: Vec<PathBuf>,
    album_title: Option<String>,
    artist: Option<String>,
    disc_id: Option<String>,
    year: Option<i32>,
    track_count: usize,
    track_titles_by_number: HashMap<i64, String>,
}

impl AlbumMatchHints {
    fn from_request(request: &ManualImportRequest) -> Self {
        let album_title = request
            .cue_hints
            .iter()
            .filter_map(|hint| hint.album_title.as_deref())
            .find(|title| !title.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| album_title_from_download(&request.download.title))
            .or_else(|| album_title_from_download(&request.download.output_path));
        let artist = request
            .cue_hints
            .iter()
            .filter_map(|hint| hint.performer.as_deref())
            .find(|performer| !performer.trim().is_empty())
            .map(str::to_owned);
        let disc_id = request
            .cue_hints
            .iter()
            .filter_map(|hint| hint.disc_id.as_deref())
            .find(|disc_id| !disc_id.trim().is_empty())
            .map(|disc_id| disc_id.trim().to_owned());
        let year = request
            .cue_hints
            .iter()
            .find_map(year_from_cue_hint)
            .or_else(|| first_year(&request.download.title))
            .or_else(|| first_year(&request.download.output_path));
        let track_count = request
            .cue_hints
            .iter()
            .map(|hint| hint.track_count)
            .sum::<usize>()
            .max(request.generated_tracks.len());
        let mut track_titles_by_number = HashMap::new();
        for hint in &request.cue_hints {
            for track in &hint.tracks {
                let Some(number) = parse_track_number(&track.number) else {
                    continue;
                };
                let Some(title) = track
                    .title
                    .as_ref()
                    .filter(|title| !title.trim().is_empty())
                else {
                    continue;
                };
                track_titles_by_number.insert(number, title.clone());
            }
        }

        Self {
            cue_paths: request
                .cue_hints
                .iter()
                .map(|hint| hint.path.clone())
                .collect(),
            album_title,
            artist,
            disc_id,
            year,
            track_count,
            track_titles_by_number,
        }
    }
}

fn append_album_match_hints(diagnostic: &mut String, hints: &AlbumMatchHints) {
    diagnostic.push_str(&format!(
        "Fallback hints: album={} artist={} year={} track_count={}\n",
        hints.album_title.as_deref().unwrap_or("-"),
        hints.artist.as_deref().unwrap_or("-"),
        hints
            .year
            .map(|year| year.to_string())
            .unwrap_or_else(|| "-".into()),
        hints.track_count
    ));
    diagnostic.push_str(&format!(
        "Fallback CUE DISCID: {}\n",
        hints.disc_id.as_deref().unwrap_or("-")
    ));
    if !hints.track_titles_by_number.is_empty() {
        diagnostic.push_str("Fallback CUE tracks:\n");
        let mut tracks = hints.track_titles_by_number.iter().collect::<Vec<_>>();
        tracks.sort_by_key(|(number, _)| *number);
        for (number, title) in tracks {
            diagnostic.push_str(&format!("  - {number}: {title}\n"));
        }
    }
}

struct FallbackAlbumMatch<'a> {
    album: &'a LidarrAlbumResource,
    release: &'a LidarrAlbumRelease,
}

struct SelectedFallbackAlbum {
    album_id: i64,
    album_release_id: i64,
}

struct FallbackAlbumCandidate<'a> {
    album: &'a LidarrAlbumResource,
    releases: Vec<&'a LidarrAlbumRelease>,
    requires_disc_lookup: bool,
}

fn select_fallback_album_candidate<'a>(
    albums: &'a [LidarrAlbumResource],
    hints: &AlbumMatchHints,
    diagnostic: &mut String,
) -> std::result::Result<FallbackAlbumCandidate<'a>, String> {
    let Some(album_title) = hints.album_title.as_deref() else {
        return Err("fallback requires an album title hint".into());
    };
    let normalized_hint = normalize_match_text(album_title);
    if normalized_hint.is_empty() {
        return Err("fallback album title hint normalized to empty text".into());
    }

    diagnostic.push_str("Fallback album candidates:\n");
    let mut matches = Vec::new();
    for album in albums {
        let normalized_album = normalize_match_text(&album.title);
        let title_match = titles_strongly_match(&normalized_hint, &normalized_album);
        let release_year = album.release_date.as_deref().and_then(first_year);
        let year_match = hints
            .year
            .zip(release_year)
            .is_some_and(|(hint_year, album_year)| hint_year == album_year);
        let year_compatible = match (hints.year, release_year) {
            (Some(_), Some(_)) => year_match,
            (Some(_), None) => false,
            (None, _) => true,
        };
        let releases = matching_releases(album, hints.track_count);
        let release_candidates = if releases.is_empty() && hints.disc_id.is_some() {
            album.releases.iter().collect::<Vec<_>>()
        } else {
            releases.clone()
        };
        let requires_disc_lookup = releases.is_empty();
        diagnostic.push_str(&format!(
            "  - album_id={} title={} normalized={} release_date={} title_match={} year_match={} matching_releases={} disc_lookup_release_candidates={}\n",
            album.id,
            album.title,
            normalized_album,
            album.release_date.as_deref().unwrap_or("-"),
            title_match,
            year_match,
            releases.len(),
            release_candidates.len()
        ));
        for release in &release_candidates {
            diagnostic.push_str(&format!(
                "      release_id={} album_id={} foreign_release_id={} title={} track_count={} monitored={}\n",
                release.id,
                release.album_id,
                release.foreign_release_id.as_deref().unwrap_or("-"),
                release.title.as_deref().unwrap_or("-"),
                release.track_count,
                release.monitored
            ));
        }
        if title_match && year_compatible && !release_candidates.is_empty() {
            matches.push(FallbackAlbumCandidate {
                album,
                releases: release_candidates,
                requires_disc_lookup,
            });
        }
    }

    if matches.is_empty() {
        return Err("no Lidarr album matched title/year/track-count hints".into());
    }
    if matches.len() > 1 {
        return Err("multiple Lidarr albums matched fallback hints".into());
    }

    Ok(matches.remove(0))
}

fn matching_releases(album: &LidarrAlbumResource, track_count: usize) -> Vec<&LidarrAlbumRelease> {
    album
        .releases
        .iter()
        .filter(|release| release.track_count == track_count)
        .collect()
}

fn lidarr_source_artist(albums: &[LidarrAlbumResource], artist_id: i64) -> Option<&Value> {
    albums
        .iter()
        .find(|album| album.artist_id == artist_id)
        .and_then(|album| album.artist.as_ref())
        .or_else(|| albums.iter().find_map(|album| album.artist.as_ref()))
}

fn select_fallback_release(releases: Vec<&LidarrAlbumRelease>) -> Option<&LidarrAlbumRelease> {
    if releases.len() == 1 {
        return releases.into_iter().next();
    }
    let monitored = releases
        .iter()
        .copied()
        .filter(|release| release.monitored)
        .collect::<Vec<_>>();
    if monitored.len() == 1 {
        return monitored.into_iter().next();
    }
    None
}

fn map_generated_tracks_to_lidarr_tracks(
    request: &ManualImportRequest,
    tracks: &[LidarrTrackResource],
    diagnostic: &mut String,
) -> std::result::Result<HashMap<String, i64>, String> {
    let cue_titles = AlbumMatchHints::from_request(request).track_titles_by_number;
    let mut mapped = HashMap::new();
    let mut used_track_ids = HashSet::new();

    diagnostic.push_str("Fallback Lidarr tracks:\n");
    for track in tracks {
        diagnostic.push_str(&format!(
            "  - track_id={} album_id={} absolute={} track_number={} title={}\n",
            track.id,
            track.album_id,
            track.absolute_track_number,
            track.track_number.as_deref().unwrap_or("-"),
            track.title
        ));
    }
    diagnostic.push_str("Fallback track mapping:\n");

    for generated_track in &request.generated_tracks {
        let parsed = parse_generated_track(generated_track);
        let Some(number) = parsed.number else {
            return Err(format!(
                "{} did not contain a parseable track number",
                generated_track.display()
            ));
        };
        let generated_title = cue_titles
            .get(&number)
            .cloned()
            .or(parsed.title)
            .unwrap_or_default();
        let normalized_generated = normalize_track_title(&generated_title);
        let matches = tracks
            .iter()
            .filter(|track| lidarr_track_number(track) == Some(number))
            .filter_map(|track| {
                let normalized_lidarr = normalize_track_title(&track.title);
                track_title_match_kind(&normalized_generated, &normalized_lidarr)
                    .map(|match_kind| (track, normalized_lidarr, match_kind))
            })
            .collect::<Vec<_>>();

        diagnostic.push_str(&format!(
            "  - generated={} number={} title={} normalized={} matches={}\n",
            generated_track.display(),
            number,
            generated_title,
            normalized_generated,
            matches.len()
        ));
        for (matched, normalized_lidarr, match_kind) in &matches {
            diagnostic.push_str(&format!(
                "      matched track_id={} title={} normalized={} match={}\n",
                matched.id, matched.title, normalized_lidarr, match_kind
            ));
        }

        if matches.len() != 1 {
            return Err(format!(
                "{} did not map uniquely to a Lidarr track",
                generated_track.display()
            ));
        }
        let track_id = matches[0].0.id;
        if !used_track_ids.insert(track_id) {
            return Err(format!(
                "Lidarr track {track_id} was matched more than once"
            ));
        }
        mapped.insert(generated_track.to_string_lossy().to_string(), track_id);
    }

    Ok(mapped)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GeneratedTrackHint {
    number: Option<i64>,
    title: Option<String>,
}

fn parse_generated_track(path: &Path) -> GeneratedTrackHint {
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return GeneratedTrackHint {
            number: None,
            title: None,
        };
    };
    let parts = stem
        .split(" - ")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    for (index, part) in parts.iter().enumerate().rev() {
        let Some(number) = parse_track_number(part) else {
            continue;
        };
        let title = parts.get(index + 1..).and_then(|title_parts| {
            let title = title_parts.join(" - ");
            (!title.trim().is_empty()).then_some(title)
        });
        return GeneratedTrackHint {
            number: Some(number),
            title,
        };
    }

    GeneratedTrackHint {
        number: None,
        title: Some(stem.to_owned()),
    }
}

fn lidarr_track_number(track: &LidarrTrackResource) -> Option<i64> {
    track
        .track_number
        .as_deref()
        .and_then(parse_track_number)
        .or_else(|| (track.absolute_track_number > 0).then_some(track.absolute_track_number))
}

fn parse_track_number(value: &str) -> Option<i64> {
    let digits = value
        .chars()
        .skip_while(|ch| !ch.is_ascii_digit())
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn year_from_cue_hint(hint: &CueMetadataHint) -> Option<i32> {
    hint.comments
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("date"))
        .and_then(|(_, value)| first_year(value))
}

fn first_year(value: &str) -> Option<i32> {
    value
        .as_bytes()
        .windows(4)
        .filter_map(|window| std::str::from_utf8(window).ok())
        .find_map(|candidate| {
            let year = candidate.parse::<i32>().ok()?;
            (1900..=2100).contains(&year).then_some(year)
        })
}

fn album_title_from_download(value: &str) -> Option<String> {
    let stem = Path::new(value)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(value);
    let without_year = remove_parenthetical_year(stem);
    let parts = without_year
        .split(" - ")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    parts
        .last()
        .map(|part| (*part).to_owned())
        .filter(|part| !part.is_empty())
}

fn remove_parenthetical_year(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '(' {
            let mut fragment = String::new();
            while let Some(next) = chars.peek().copied() {
                chars.next();
                if next == ')' {
                    break;
                }
                fragment.push(next);
            }
            if first_year(&fragment).is_some() {
                continue;
            }
            out.push(' ');
            out.push_str(&fragment);
            out.push(' ');
            continue;
        }
        out.push(ch);
    }
    out
}

fn normalize_track_title(value: &str) -> String {
    normalize_match_text(&remove_parenthetical_year(value))
}

fn normalize_match_text(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            normalized.push(ch);
        } else {
            normalized.push(' ');
        }
    }
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn titles_strongly_match(left: &str, right: &str) -> bool {
    if left.is_empty() || right.is_empty() {
        return false;
    }
    if left == right {
        return true;
    }
    let left_tokens = significant_tokens(left);
    let right_tokens = significant_tokens(right);
    !left_tokens.is_empty()
        && !right_tokens.is_empty()
        && (left_tokens.is_subset(&right_tokens) || right_tokens.is_subset(&left_tokens))
}

fn significant_tokens(value: &str) -> HashSet<String> {
    value
        .split_whitespace()
        .filter(|token| !matches!(*token, "the" | "a" | "an"))
        .map(str::to_owned)
        .collect()
}

fn track_title_match_kind(left: &str, right: &str) -> Option<&'static str> {
    if left.is_empty() || right.is_empty() {
        return None;
    }
    if left == right {
        return Some("exact");
    }
    if titles_strongly_match(left, right) {
        return Some("strong");
    }

    let relaxed_left = relax_dropped_g_suffixes(left);
    let relaxed_right = relax_dropped_g_suffixes(right);
    if (relaxed_left != left || relaxed_right != right)
        && titles_strongly_match(&relaxed_left, &relaxed_right)
    {
        return Some("relaxed-dropped-g");
    }

    None
}

fn relax_dropped_g_suffixes(value: &str) -> String {
    value
        .split_whitespace()
        .map(|token| {
            token
                .strip_suffix("ing")
                .filter(|stem| stem.len() >= 2)
                .map_or_else(|| token.to_owned(), |stem| format!("{stem}in"))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use serde_json::Value;
    use tempfile::tempdir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::LidarrQueueSource;
    use crate::application::ports::{
        CueMetadataHint, CueTrackHint, DiscReleaseCandidate, DiscReleaseLookup,
        DiscReleaseLookupRequest, DiscReleaseLookupResult, ManualImportRequest, ManualImportResult,
        ManualImportTrigger, MusicBrainzDiscLookupRequest, MusicBrainzDiscLookupResult,
        MusicBrainzDiscRelease, MusicBrainzDiscReleaseLookup, QueueSource,
    };
    use crate::bootstrap::settings::LidarrSettings;
    use crate::domain::{FailedImportCandidate, TrackedDownload};

    #[test]
    fn queue_record_candidate_requires_import_failed_completed_with_path() {
        let record = super::QueueRecord {
            title: Some("Album".to_owned()),
            status: Some("completed".to_owned()),
            tracked_download_state: Some("importFailed".to_owned()),
            download_id: Some("abc".to_owned()),
            output_path: Some("/downloads/album".to_owned()),
        };

        assert_eq!(
            record.as_candidate(),
            Some(FailedImportCandidate {
                download_id: "abc".to_owned(),
                title: "Album".to_owned(),
                status: "completed".to_owned(),
                output_path: "/downloads/album".to_owned(),
                tracked_download_state: "importFailed".to_owned(),
            })
        );

        let mut missing_path = record;
        missing_path.output_path = None;
        assert_eq!(missing_path.as_candidate(), None);
    }

    #[tokio::test]
    async fn client_parses_successful_queue_response() {
        let body = r#"{"records":[{"title":"Album","status":"completed","trackedDownloadState":"importFailed","downloadId":"abc","outputPath":"/downloads/album"}]}"#;
        let url = serve_once("200 OK", body).await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 100,
            queue_max_pages: 100,
            manual_import_enabled: false,
        });

        let queue = client.queue_snapshot().await.unwrap();

        assert_eq!(queue.total_records, 1);
        assert_eq!(queue.pages_fetched, 1);
        assert!(queue.active_download_ids.contains("abc"));
    }

    #[tokio::test]
    async fn client_fetches_multiple_pages_and_collects_failed_imports() {
        let (url, _) = serve_sequence(vec![
            (
                "200 OK",
                r#"{"totalRecords":2,"records":[{"title":"Downloading","status":"warning","trackedDownloadState":"downloading","downloadId":"down-1","outputPath":"/downloads/down-1"}]}"#,
            ),
            (
                "200 OK",
                r#"{"totalRecords":2,"records":[{"title":"Failed","status":"completed","trackedDownloadState":"importFailed","downloadId":"fail-1","outputPath":"/downloads/fail-1"}]}"#,
            ),
        ])
        .await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 1,
            queue_max_pages: 100,
            manual_import_enabled: false,
        });

        let queue = client.queue_snapshot().await.unwrap();

        assert_eq!(queue.total_records, 2);
        assert_eq!(queue.pages_fetched, 2);
        assert!(queue.active_download_ids.contains("down-1"));
        assert!(queue.active_download_ids.contains("fail-1"));
        assert_eq!(queue.failed_imports.len(), 1);
    }

    #[tokio::test]
    async fn client_stops_when_short_page_is_returned() {
        let (url, requests) = serve_sequence(vec![(
            "200 OK",
            r#"{"records":[{"title":"Album","status":"completed","trackedDownloadState":"importFailed","downloadId":"abc","outputPath":"/downloads/album"}]}"#,
        )])
        .await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 100,
            queue_max_pages: 100,
            manual_import_enabled: false,
        });

        let queue = client.queue_snapshot().await.unwrap();
        assert_eq!(queue.pages_fetched, 1);
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn client_stops_when_empty_page_is_returned() {
        let (url, requests) = serve_sequence(vec![
            (
                "200 OK",
                r#"{"totalRecords":5,"records":[{"downloadId":"a"}]}"#,
            ),
            ("200 OK", r#"{"totalRecords":5,"records":[]}"#),
        ])
        .await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 1,
            queue_max_pages: 100,
            manual_import_enabled: false,
        });

        let queue = client.queue_snapshot().await.unwrap();
        assert_eq!(queue.pages_fetched, 2);
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn client_reports_non_success_status() {
        let url = serve_once("500 Internal Server Error", "boom").await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 100,
            queue_max_pages: 100,
            manual_import_enabled: false,
        });

        let err = client.queue_snapshot().await.unwrap_err();

        assert!(err.to_string().contains("HTTP 500"));
        assert!(err.to_string().contains("page 1"));
    }

    #[tokio::test]
    async fn client_reports_malformed_json() {
        let url = serve_once("200 OK", "not-json").await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 100,
            queue_max_pages: 100,
            manual_import_enabled: false,
        });

        let err = client.queue_snapshot().await.unwrap_err();

        assert!(err.to_string().contains("invalid queue JSON"));
        assert!(err.to_string().contains("page 1"));
    }

    #[tokio::test]
    async fn client_errors_when_max_pages_is_exceeded() {
        let (url, _) = serve_sequence(vec![
            (
                "200 OK",
                r#"{"totalRecords":999,"records":[{"downloadId":"a"}]}"#,
            ),
            (
                "200 OK",
                r#"{"totalRecords":999,"records":[{"downloadId":"b"}]}"#,
            ),
            (
                "200 OK",
                r#"{"totalRecords":999,"records":[{"downloadId":"c"}]}"#,
            ),
        ])
        .await;
        let client = LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 1,
            queue_max_pages: 2,
            manual_import_enabled: false,
        });

        let err = client.queue_snapshot().await.unwrap_err();
        assert!(err.to_string().contains("exceeded max pages"));
    }

    #[tokio::test]
    async fn manual_import_disabled_makes_no_request() {
        let (url, requests) = serve_sequence(Vec::new()).await;
        let client = lidarr_client(url, false);

        let result = client
            .trigger_manual_import(manual_import_request(vec!["/downloads/album/01.flac"], 1))
            .await
            .unwrap();

        assert_eq!(result, ManualImportResult::Disabled);
        assert!(requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn manual_import_starts_command_for_one_album_release() {
        let candidates = r#"[
            {"path":"/downloads/album/01.flac","artist":{"id":1,"artistName":"Artist"},"album":{"id":2,"title":"Album"},"albumReleaseId":3,"tracks":[{"id":11}],"quality":{"quality":{"id":6,"name":"FLAC"},"revision":{"version":1,"real":0}},"indexerFlags":0},
            {"path":"/downloads/album/02.flac","artist":{"id":1,"artistName":"Artist"},"album":{"id":2,"title":"Album"},"albumReleaseId":3,"tracks":[{"id":12}],"quality":{"quality":{"id":6,"name":"FLAC"},"revision":{"version":1,"real":0}},"indexerFlags":0}
        ]"#;
        let (url, requests) =
            serve_sequence(vec![("200 OK", candidates), ("201 Created", r#"{"id":7}"#)]).await;
        let client = lidarr_client(url, true);

        let result = client
            .trigger_manual_import(manual_import_request(
                vec!["/downloads/album/01.flac", "/downloads/album/02.flac"],
                2,
            ))
            .await
            .unwrap();

        let ManualImportResult::Started {
            imported_track_count,
            diagnostic,
        } = result
        else {
            panic!("expected manual import to start");
        };
        assert_eq!(imported_track_count, 2);
        assert!(diagnostic.contains("Generated split tracks"));
        assert!(diagnostic.contains("album_release_id=3"));
        assert!(diagnostic.contains("track_ids=[11]"));
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("GET /api/v1/manualimport?"));
        assert!(requests[0].contains("downloadId=download-1"));
        assert!(requests[1].starts_with("POST /api/v1/command "));

        let body = request_body(&requests[1]);
        let command: Value = serde_json::from_str(body).unwrap();
        assert_eq!(command["name"], "ManualImport");
        assert_eq!(command["importMode"], "Move");
        assert_eq!(command["replaceExistingFiles"], true);
        assert_eq!(command["files"].as_array().unwrap().len(), 2);
        assert_eq!(command["files"][0]["albumReleaseId"], 3);
        assert_eq!(command["files"][0]["trackIds"], serde_json::json!([11]));
        assert_eq!(command["files"][0]["downloadId"], "download-1");
    }

    #[tokio::test]
    async fn manual_import_polls_started_command_until_completed() {
        let candidates = r#"[
            {"path":"/downloads/album/01.flac","artist":{"id":1,"artistName":"Artist"},"album":{"id":2,"title":"Album"},"albumReleaseId":3,"tracks":[{"id":11}],"quality":{"quality":{"id":6,"name":"FLAC"}},"indexerFlags":0}
        ]"#;
        let started = r#"{"id":7,"name":"ManualImport","commandName":"Manual Import","status":"started","result":"unknown","message":"Importing"}"#;
        let completed = r#"{"id":7,"name":"ManualImport","commandName":"Manual Import","status":"completed","result":"successful","message":"Completed"}"#;
        let (url, requests) = serve_sequence(vec![
            ("200 OK", candidates),
            ("201 Created", started),
            ("200 OK", completed),
        ])
        .await;
        let client = lidarr_client(url, true).with_lidarr_command_poll(3, Duration::ZERO);

        let result = client
            .trigger_manual_import(manual_import_request(vec!["/downloads/album/01.flac"], 1))
            .await
            .unwrap();

        let ManualImportResult::Started { diagnostic, .. } = result else {
            panic!("expected manual import to start");
        };
        assert!(diagnostic.contains("Lidarr manual import command accepted: id=7"));
        assert!(diagnostic.contains("Lidarr manual import command poll attempt=1: id=7"));
        assert!(
            diagnostic.contains("Lidarr manual import command decision: completed successfully")
        );
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[2].starts_with("GET /api/v1/command/7 "));
    }

    #[tokio::test]
    async fn manual_import_reports_started_command_failure() {
        let candidates = r#"[
            {"path":"/downloads/album/01.flac","artist":{"id":1,"artistName":"Artist"},"album":{"id":2,"title":"Album"},"albumReleaseId":3,"tracks":[{"id":11}],"quality":{"quality":{"id":6,"name":"FLAC"}},"indexerFlags":0}
        ]"#;
        let started = r#"{"id":7,"name":"ManualImport","commandName":"Manual Import","status":"started","result":"unknown","message":"Importing"}"#;
        let failed = r#"{"id":7,"name":"ManualImport","commandName":"Manual Import","status":"failed","result":"unsuccessful","message":"Failed","exception":"track import exploded"}"#;
        let (url, requests) = serve_sequence(vec![
            ("200 OK", candidates),
            ("201 Created", started),
            ("200 OK", failed),
        ])
        .await;
        let client = lidarr_client(url, true).with_lidarr_command_poll(3, Duration::ZERO);

        let err = client
            .trigger_manual_import(manual_import_request(vec!["/downloads/album/01.flac"], 1))
            .await
            .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("lidarr manual import command failed"));
        assert!(message.contains("track import exploded"));
        assert!(message.contains("Lidarr manual import command poll attempt=1"));
        assert!(message.contains("track_ids=[11]"));
        assert_eq!(requests.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn manual_import_reports_completed_command_with_remaining_sources() {
        let tmp = tempdir().unwrap();
        let source = tmp.path().join("Artist - Album - 01 - One.flac");
        fs::write(&source, b"audio").unwrap();
        let source = source.to_string_lossy().to_string();
        let candidates = serde_json::json!([
            {
                "path": source,
                "artist": {"id": 1, "artistName": "Artist"},
                "album": {"id": 2, "title": "Album"},
                "albumReleaseId": 3,
                "tracks": [{"id": 11}],
                "quality": {"quality": {"id": 6, "name": "FLAC"}},
                "indexerFlags": 0
            }
        ])
        .to_string();
        let candidates = Box::leak(candidates.into_boxed_str());
        let completed = r#"{"id":7,"name":"ManualImport","commandName":"Manual Import","status":"completed","result":"successful","message":"Manually imported 1 files"}"#;
        let (url, requests) =
            serve_sequence(vec![("200 OK", candidates), ("201 Created", completed)]).await;
        let client = lidarr_client(url, true).with_lidarr_command_poll(3, Duration::ZERO);

        let err = client
            .trigger_manual_import(manual_import_request(vec![&source], 1))
            .await
            .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("reported success"));
        assert!(message.contains("still exist at source"));
        assert!(message.contains("Lidarr manual import source verification"));
        assert!(message.contains(&source));
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn manual_import_skips_multiple_album_releases() {
        let candidates = r#"[
            {"path":"/downloads/album/01.flac","artist":{"id":1},"album":{"id":2,"title":"Album"},"albumReleaseId":3,"tracks":[{"id":11}],"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/02.flac","artist":{"id":1},"album":{"id":2,"title":"Album"},"albumReleaseId":4,"tracks":[{"id":12}],"quality":{"quality":{"id":6}}}
        ]"#;
        let (url, requests) = serve_sequence(vec![("200 OK", candidates)]).await;
        let client = lidarr_client(url, true);

        let result = client
            .trigger_manual_import(manual_import_request(
                vec!["/downloads/album/01.flac", "/downloads/album/02.flac"],
                2,
            ))
            .await
            .unwrap();

        let ManualImportResult::Skipped { reason, diagnostic } = result else {
            panic!("expected manual import to skip");
        };
        assert!(reason.contains("one album release"));
        assert!(diagnostic.contains("album_release_id=3"));
        assert!(diagnostic.contains("album_release_id=4"));
        assert!(diagnostic.contains("Decision: skipped manual import"));
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn manual_import_skips_incomplete_candidates() {
        let candidates = r#"[
            {"path":"/downloads/album/01.flac","artist":{"id":1},"album":{"id":2,"title":"Album"},"albumReleaseId":3,"tracks":[{"id":11}]}
        ]"#;
        let (url, requests) = serve_sequence(vec![("200 OK", candidates)]).await;
        let client = lidarr_client(url, true);

        let result = client
            .trigger_manual_import(manual_import_request(vec!["/downloads/album/01.flac"], 1))
            .await
            .unwrap();

        let ManualImportResult::Skipped { reason, diagnostic } = result else {
            panic!("expected manual import to skip");
        };
        assert!(reason.contains("missing quality"));
        assert!(diagnostic.contains("quality_present=false"));
        assert!(diagnostic.contains("/downloads/album/01.flac"));
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn manual_import_reports_api_errors() {
        let url = serve_once("500 Internal Server Error", "boom").await;
        let client = lidarr_client(url, true);

        let err = client
            .trigger_manual_import(manual_import_request(vec!["/downloads/album/01.flac"], 1))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("HTTP 500"));
        assert!(err.to_string().contains("manual import candidates"));
    }

    #[tokio::test]
    async fn manual_import_command_errors_include_diagnostic() {
        let candidates = r#"[
            {"path":"/downloads/album/01.flac","artist":{"id":1,"artistName":"Artist"},"album":{"id":2,"title":"Album"},"albumReleaseId":3,"tracks":[{"id":11}],"quality":{"quality":{"id":6,"name":"FLAC"}},"indexerFlags":0}
        ]"#;
        let (url, _) = serve_sequence(vec![
            ("200 OK", candidates),
            ("500 Internal Server Error", "boom"),
        ])
        .await;
        let client = lidarr_client(url, true);

        let err = client
            .trigger_manual_import(manual_import_request(vec!["/downloads/album/01.flac"], 1))
            .await
            .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("HTTP 500"));
        assert!(message.contains("Manual import diagnostic"));
        assert!(message.contains("track_ids=[11]"));
    }

    #[test]
    fn fallback_normalization_tolerates_common_filename_noise() {
        assert_eq!(
            super::normalize_match_text("Kalimba_De Luna... (1984)"),
            "kalimba de luna 1984"
        );
        assert_eq!(
            super::normalize_track_title("The Calendar Song _January_ February_ March_"),
            "the calendar song january february march"
        );
        assert!(super::titles_strongly_match(
            &super::normalize_track_title("The Calendar Song (January February March)"),
            &super::normalize_track_title("Calendar Song _January_ February_ March_")
        ));
        assert_eq!(
            super::track_title_match_kind(
                &super::normalize_track_title("Hold On I'm Coming"),
                &super::normalize_track_title("Hold On! I\u{2019}m Comin\u{2019}")
            ),
            Some("relaxed-dropped-g")
        );
    }

    #[tokio::test]
    async fn manual_import_fallback_infers_album_release_and_tracks() {
        let candidates = r#"[
            {"path":"/downloads/Boney M. - Oceans Of Fantasy(1979)/Boney M. - Oceans Of Fantasy - 01 - Let It All Be Music.flac","artist":{"id":16,"artistName":"Boney M."},"quality":{"quality":{"id":6,"name":"FLAC"}},"indexerFlags":2},
            {"path":"/downloads/Boney M. - Oceans Of Fantasy(1979)/Boney M. - Oceans Of Fantasy - 02 - Gotta Go Home.flac","artist":{"id":16,"artistName":"Boney M."},"quality":{"quality":{"id":6,"name":"FLAC"}},"indexerFlags":2}
        ]"#;
        let albums = r#"[
            {"id":22,"title":"Oceans Of Fantasy","artistId":16,"releaseDate":"1979-01-01T00:00:00Z","releases":[{"id":33,"albumId":22,"title":"Oceans Of Fantasy","trackCount":2,"monitored":true}]}
        ]"#;
        let tracks = r#"[
            {"id":101,"albumId":22,"absoluteTrackNumber":1,"trackNumber":"1","title":"Let It All Be Music"},
            {"id":102,"albumId":22,"absoluteTrackNumber":2,"trackNumber":"2","title":"Gotta Go Home"}
        ]"#;
        let (url, requests) = serve_sequence(vec![
            ("200 OK", candidates),
            ("200 OK", albums),
            ("200 OK", tracks),
            ("201 Created", r#"{"id":7}"#),
        ])
        .await;
        let client = lidarr_client(url, true);

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata(
                "Boney M. - Oceans Of Fantasy",
                "/downloads/Boney M. - Oceans Of Fantasy(1979)",
                "Oceans Of Fantasy",
                "Boney M.",
                "1979",
                vec![
                    (
                        "/downloads/Boney M. - Oceans Of Fantasy(1979)/Boney M. - Oceans Of Fantasy - 01 - Let It All Be Music.flac",
                        "Let It All Be Music",
                    ),
                    (
                        "/downloads/Boney M. - Oceans Of Fantasy(1979)/Boney M. - Oceans Of Fantasy - 02 - Gotta Go Home.flac",
                        "Gotta Go Home",
                    ),
                ],
            ))
            .await
            .unwrap();

        let ManualImportResult::Started {
            imported_track_count,
            diagnostic,
        } = result
        else {
            panic!("expected manual import to start");
        };
        assert_eq!(imported_track_count, 2);
        assert!(diagnostic.contains("Fallback hints: album=Oceans Of Fantasy"));
        assert!(diagnostic.contains("Fallback selected album: album_id=22"));
        assert!(diagnostic.contains("matched track_id=101"));
        assert!(diagnostic.contains("matched track_id=102"));

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[1].starts_with("GET /api/v1/album?"));
        assert!(requests[1].contains("artistId=16"));
        assert!(requests[2].starts_with("GET /api/v1/track?"));
        assert!(requests[2].contains("albumId=22"));
        let command: Value = serde_json::from_str(request_body(&requests[3])).unwrap();
        assert_eq!(command["files"][0]["artistId"], 16);
        assert_eq!(command["files"][0]["albumId"], 22);
        assert_eq!(command["files"][0]["albumReleaseId"], 33);
        assert_eq!(command["files"][0]["trackIds"], serde_json::json!([101]));
        assert_eq!(command["files"][1]["trackIds"], serde_json::json!([102]));
        assert_eq!(command["files"][0]["indexerFlags"], 2);
    }

    #[tokio::test]
    async fn manual_import_fallback_maps_dropped_g_track_title_variant() {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - Hold On I'm Coming.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6,"name":"FLAC"}},"indexerFlags":0}
        ]"#;
        let albums = r#"[
            {"id":2,"title":"Album","artistId":1,"releaseDate":"1979-01-01","releases":[{"id":3,"albumId":2,"title":"Album","trackCount":1,"monitored":true}]}
        ]"#;
        let tracks = r#"[
            {"id":11,"albumId":2,"absoluteTrackNumber":1,"trackNumber":"1","title":"Hold On! I\u2019m Comin\u2019"}
        ]"#;
        let (url, requests) = serve_sequence(vec![
            ("200 OK", candidates),
            ("200 OK", albums),
            ("200 OK", tracks),
            ("201 Created", r#"{"id":7}"#),
        ])
        .await;
        let client = lidarr_client(url, true);

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1979",
                vec![(
                    "/downloads/album/Artist - Album - 01 - Hold On I'm Coming.flac",
                    "Hold On I'm Coming",
                )],
            ))
            .await
            .unwrap();

        let ManualImportResult::Started {
            imported_track_count,
            diagnostic,
        } = result
        else {
            panic!("expected manual import to start");
        };
        assert_eq!(imported_track_count, 1);
        assert!(diagnostic.contains("match=relaxed-dropped-g"));
        assert!(diagnostic.contains("matched track_id=11"));

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        let command: Value = serde_json::from_str(request_body(&requests[3])).unwrap();
        assert_eq!(command["files"][0]["trackIds"], serde_json::json!([11]));
    }

    #[tokio::test]
    async fn manual_import_fallback_skips_multiple_album_matches() {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let albums = r#"[
            {"id":2,"title":"Album","artistId":1,"releaseDate":"1984-01-01","releases":[{"id":3,"albumId":2,"trackCount":1}]},
            {"id":4,"title":"Album","artistId":1,"releaseDate":"1984-01-01","releases":[{"id":5,"albumId":4,"trackCount":1}]}
        ]"#;
        let (url, requests) =
            serve_sequence(vec![("200 OK", candidates), ("200 OK", albums)]).await;
        let client = lidarr_client(url, true);

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1984",
                vec![("/downloads/album/Artist - Album - 01 - One.flac", "One")],
            ))
            .await
            .unwrap();

        let ManualImportResult::Skipped { reason, diagnostic } = result else {
            panic!("expected manual import to skip");
        };
        assert!(reason.contains("multiple Lidarr albums"));
        assert!(diagnostic.contains("Fallback album candidates"));
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn manual_import_fallback_skips_non_unique_track_matches() {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let albums = r#"[
            {"id":2,"title":"Album","artistId":1,"releaseDate":"1984-01-01","releases":[{"id":3,"albumId":2,"trackCount":1}]}
        ]"#;
        let tracks = r#"[
            {"id":11,"albumId":2,"absoluteTrackNumber":1,"trackNumber":"1","title":"One"},
            {"id":12,"albumId":2,"absoluteTrackNumber":1,"trackNumber":"1","title":"One"}
        ]"#;
        let (url, requests) = serve_sequence(vec![
            ("200 OK", candidates),
            ("200 OK", albums),
            ("200 OK", tracks),
        ])
        .await;
        let client = lidarr_client(url, true);

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1984",
                vec![("/downloads/album/Artist - Album - 01 - One.flac", "One")],
            ))
            .await
            .unwrap();

        let ManualImportResult::Skipped { reason, diagnostic } = result else {
            panic!("expected manual import to skip");
        };
        assert!(reason.contains("did not map uniquely"));
        assert!(diagnostic.contains("matches=2"));
        assert_eq!(requests.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn manual_import_fallback_skips_when_no_release_has_expected_track_count() {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let albums = r#"[
            {"id":2,"title":"Album","artistId":1,"releaseDate":"1984-01-01","releases":[{"id":3,"albumId":2,"trackCount":2}]}
        ]"#;
        let (url, requests) =
            serve_sequence(vec![("200 OK", candidates), ("200 OK", albums)]).await;
        let client = lidarr_client(url, true);

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1984",
                vec![("/downloads/album/Artist - Album - 01 - One.flac", "One")],
            ))
            .await
            .unwrap();

        let ManualImportResult::Skipped { reason, diagnostic } = result else {
            panic!("expected manual import to skip");
        };
        assert!(reason.contains("no Lidarr album matched"));
        assert!(diagnostic.contains("matching_releases=0"));
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn manual_import_fallback_skips_partial_track_mapping() {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/Artist - Album - 02 - Two.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let albums = r#"[
            {"id":2,"title":"Album","artistId":1,"releaseDate":"1984-01-01","releases":[{"id":3,"albumId":2,"trackCount":2}]}
        ]"#;
        let tracks = r#"[
            {"id":11,"albumId":2,"absoluteTrackNumber":1,"trackNumber":"1","title":"One"},
            {"id":12,"albumId":2,"absoluteTrackNumber":2,"trackNumber":"2","title":"Different"}
        ]"#;
        let (url, requests) = serve_sequence(vec![
            ("200 OK", candidates),
            ("200 OK", albums),
            ("200 OK", tracks),
        ])
        .await;
        let client = lidarr_client(url, true);

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1984",
                vec![
                    ("/downloads/album/Artist - Album - 01 - One.flac", "One"),
                    ("/downloads/album/Artist - Album - 02 - Two.flac", "Two"),
                ],
            ))
            .await
            .unwrap();

        let ManualImportResult::Skipped { reason, diagnostic } = result else {
            panic!("expected manual import to skip");
        };
        assert!(reason.contains("did not map uniquely"));
        assert!(diagnostic.contains("title=Two"));
        assert!(diagnostic.contains("matches=0"));
        assert_eq!(requests.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn manual_import_fallback_uses_gnudb_artid_to_select_lidarr_release() {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/Artist - Album - 02 - Two.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let albums = r#"[
            {"id":2,"title":"Album","artistId":1,"releaseDate":"1984-01-01","releases":[
                {"id":3,"albumId":2,"foreignReleaseId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Album","trackCount":2,"monitored":false},
                {"id":4,"albumId":2,"foreignReleaseId":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb","title":"Album","trackCount":2,"monitored":false}
            ]}
        ]"#;
        let tracks = r#"[
            {"id":11,"albumId":2,"absoluteTrackNumber":1,"trackNumber":"1","title":"One"},
            {"id":12,"albumId":2,"absoluteTrackNumber":2,"trackNumber":"2","title":"Two"}
        ]"#;
        let (url, requests) = serve_sequence(vec![
            ("200 OK", candidates),
            ("200 OK", albums),
            ("200 OK", tracks),
            ("201 Created", r#"{"id":7}"#),
        ])
        .await;
        let client = lidarr_client(url, true).with_disc_release_lookup(Arc::new(FakeDiscLookup {
            result: DiscReleaseLookupResult::Found {
                candidates: vec![DiscReleaseCandidate {
                    category: "rock".into(),
                    entry_id: "c60c9d10".into(),
                    disc_id: "C60C9D10".into(),
                    artist: Some("Artist".into()),
                    title: Some("Album".into()),
                    year: Some(1984),
                    track_titles: vec!["One".into(), "Two".into()],
                    art_ids: vec!["bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".into()],
                }],
                diagnostic: "GnuDB accepted candidates: 1\n".into(),
            },
        }));

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata_and_disc_id(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1984",
                Some("C60C9D10"),
                vec![
                    ("/downloads/album/Artist - Album - 01 - One.flac", "One"),
                    ("/downloads/album/Artist - Album - 02 - Two.flac", "Two"),
                ],
            ))
            .await
            .unwrap();

        let ManualImportResult::Started {
            imported_track_count,
            diagnostic,
        } = result
        else {
            panic!("expected manual import to start");
        };
        assert_eq!(imported_track_count, 2);
        assert!(diagnostic.contains("Fallback CUE DISCID: C60C9D10"));
        assert!(diagnostic
            .contains("GnuDB selected foreign_release_id=bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"));
        let requests = requests.lock().unwrap();
        let command: Value = serde_json::from_str(request_body(&requests[3])).unwrap();
        assert_eq!(command["files"][0]["albumReleaseId"], 4);
        assert_eq!(command["files"][1]["albumReleaseId"], 4);
    }

    #[tokio::test]
    async fn manual_import_fallback_prefers_musicbrainz_release_id_before_gnudb() {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/Artist - Album - 02 - Two.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let albums = r#"[
            {"id":2,"title":"Album","artistId":1,"releaseDate":"1984-01-01","releases":[
                {"id":3,"albumId":2,"foreignReleaseId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Album","trackCount":2,"monitored":false},
                {"id":4,"albumId":2,"foreignReleaseId":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb","title":"Album","trackCount":2,"monitored":false}
            ]}
        ]"#;
        let tracks = r#"[
            {"id":11,"albumId":2,"absoluteTrackNumber":1,"trackNumber":"1","title":"One"},
            {"id":12,"albumId":2,"absoluteTrackNumber":2,"trackNumber":"2","title":"Two"}
        ]"#;
        let (url, requests) = serve_sequence(vec![
            ("200 OK", candidates),
            ("200 OK", albums),
            ("200 OK", tracks),
            ("201 Created", r#"{"id":7}"#),
        ])
        .await;
        let client = lidarr_client(url, true)
            .with_musicbrainz_disc_release_lookup(Arc::new(FakeMusicBrainzLookup {
                result: MusicBrainzDiscLookupResult::Found {
                    releases: vec![MusicBrainzDiscRelease {
                        id: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".into(),
                        title: Some("Album".into()),
                        media_count: 1,
                        media_track_counts: vec![2],
                        ..MusicBrainzDiscRelease::default()
                    }],
                    diagnostic: "MusicBrainz lookup: found 1 release(s)\n".into(),
                },
            }))
            .with_disc_release_lookup(Arc::new(FakeDiscLookup {
                result: DiscReleaseLookupResult::Found {
                    candidates: vec![DiscReleaseCandidate {
                        category: "rock".into(),
                        entry_id: "c60c9d10".into(),
                        disc_id: "C60C9D10".into(),
                        artist: Some("Artist".into()),
                        title: Some("Album".into()),
                        year: Some(1984),
                        track_titles: vec!["One".into(), "Two".into()],
                        art_ids: vec!["aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into()],
                    }],
                    diagnostic: "GnuDB should not be used\n".into(),
                },
            }));

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata_and_disc_id(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1984",
                Some("C60C9D10"),
                vec![
                    ("/downloads/album/Artist - Album - 01 - One.flac", "One"),
                    ("/downloads/album/Artist - Album - 02 - Two.flac", "Two"),
                ],
            ))
            .await
            .unwrap();

        let ManualImportResult::Started { diagnostic, .. } = result else {
            panic!("expected manual import to start");
        };
        assert!(diagnostic.contains(
            "MusicBrainz decision: exact single MBID selected foreign_release_id=bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
        ));
        assert!(!diagnostic.contains("GnuDB should not be used"));
        let command: Value =
            serde_json::from_str(request_body(&requests.lock().unwrap()[3])).unwrap();
        assert_eq!(command["files"][0]["albumReleaseId"], 4);
    }

    #[tokio::test]
    async fn manual_import_fallback_ranks_multiple_musicbrainz_exact_matches_by_monitored_release()
    {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/Artist - Album - 02 - Two.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let albums = r#"[
            {"id":2,"title":"Album","artistId":1,"releaseDate":"1984-01-01","releases":[
                {"id":3,"albumId":2,"foreignReleaseId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Album","trackCount":2,"monitored":false,"format":"CD"},
                {"id":4,"albumId":2,"foreignReleaseId":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb","title":"Album","trackCount":2,"monitored":true,"format":"CD"}
            ]}
        ]"#;
        let tracks = r#"[
            {"id":11,"albumId":2,"absoluteTrackNumber":1,"trackNumber":"1","title":"One"},
            {"id":12,"albumId":2,"absoluteTrackNumber":2,"trackNumber":"2","title":"Two"}
        ]"#;
        let (url, requests) = serve_sequence(vec![
            ("200 OK", candidates),
            ("200 OK", albums),
            ("200 OK", tracks),
            ("201 Created", r#"{"id":7}"#),
        ])
        .await;
        let client = lidarr_client(url, true)
            .with_musicbrainz_disc_release_lookup(Arc::new(FakeMusicBrainzLookup {
                result: MusicBrainzDiscLookupResult::Found {
                    releases: vec![
                        musicbrainz_release("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", 2),
                        musicbrainz_release("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", 2),
                    ],
                    diagnostic: "MusicBrainz lookup: found 2 release(s)\n".into(),
                },
            }))
            .with_disc_release_lookup(Arc::new(FakeDiscLookup {
                result: DiscReleaseLookupResult::NotFound {
                    diagnostic: "GnuDB should not be used\n".into(),
                },
            }));

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata_and_disc_id(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1984",
                Some("C60C9D10"),
                vec![
                    ("/downloads/album/Artist - Album - 01 - One.flac", "One"),
                    ("/downloads/album/Artist - Album - 02 - Two.flac", "Two"),
                ],
            ))
            .await
            .unwrap();

        let ManualImportResult::Started { diagnostic, .. } = result else {
            panic!("expected manual import to start");
        };
        assert!(diagnostic.contains("MusicBrainz exact intersection count: 2"));
        assert!(diagnostic.contains("MusicBrainz decision: ranked exact MBID selected"));
        assert!(diagnostic.contains("monitored=true"));
        assert!(!diagnostic.contains("GnuDB should not be used"));
        let command: Value =
            serde_json::from_str(request_body(&requests.lock().unwrap()[3])).unwrap();
        assert_eq!(command["files"][0]["albumReleaseId"], 4);
    }

    #[tokio::test]
    async fn manual_import_fallback_best_effort_musicbrainz_selects_monitored_compatible_release() {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/Artist - Album - 02 - Two.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let albums = r#"[
            {"id":2,"title":"Album","artistId":1,"releaseDate":"1984-01-01","releases":[
                {"id":3,"albumId":2,"foreignReleaseId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Album","trackCount":2,"monitored":false,"format":"CD"},
                {"id":4,"albumId":2,"foreignReleaseId":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb","title":"Album","trackCount":2,"monitored":true,"format":"CD"}
            ]}
        ]"#;
        let tracks = r#"[
            {"id":11,"albumId":2,"absoluteTrackNumber":1,"trackNumber":"1","title":"One"},
            {"id":12,"albumId":2,"absoluteTrackNumber":2,"trackNumber":"2","title":"Two"}
        ]"#;
        let (url, requests) = serve_sequence(vec![
            ("200 OK", candidates),
            ("200 OK", albums),
            ("200 OK", tracks),
            ("201 Created", r#"{"id":7}"#),
        ])
        .await;
        let client = lidarr_client(url, true)
            .with_musicbrainz_disc_release_lookup(Arc::new(FakeMusicBrainzLookup {
                result: MusicBrainzDiscLookupResult::Found {
                    releases: vec![musicbrainz_release(
                        "cccccccc-cccc-cccc-cccc-cccccccccccc",
                        2,
                    )],
                    diagnostic: "MusicBrainz lookup: found 1 release(s)\n".into(),
                },
            }))
            .with_disc_release_lookup(Arc::new(FakeDiscLookup {
                result: DiscReleaseLookupResult::NotFound {
                    diagnostic: "GnuDB should not be used\n".into(),
                },
            }));

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata_and_disc_id(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1984",
                Some("C60C9D10"),
                vec![
                    ("/downloads/album/Artist - Album - 01 - One.flac", "One"),
                    ("/downloads/album/Artist - Album - 02 - Two.flac", "Two"),
                ],
            ))
            .await
            .unwrap();

        let ManualImportResult::Started { diagnostic, .. } = result else {
            panic!("expected manual import to start");
        };
        assert!(diagnostic.contains("MusicBrainz exact intersection count: 0"));
        assert!(
            diagnostic.contains("MusicBrainz decision: best-effort compatible release selected")
        );
        assert!(!diagnostic.contains("GnuDB should not be used"));
        let command: Value =
            serde_json::from_str(request_body(&requests.lock().unwrap()[3])).unwrap();
        assert_eq!(command["files"][0]["albumReleaseId"], 4);
    }

    #[tokio::test]
    async fn manual_import_fallback_musicbrainz_skips_when_lidarr_releases_have_wrong_track_count()
    {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let albums = r#"[
            {"id":2,"title":"Album","artistId":1,"releaseDate":"1984-01-01","releases":[
                {"id":3,"albumId":2,"foreignReleaseId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Album","trackCount":2,"monitored":true,"format":"CD"},
                {"id":4,"albumId":2,"foreignReleaseId":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb","title":"Album","trackCount":2,"monitored":false,"format":"CD"}
            ]}
        ]"#;
        let (url, requests) =
            serve_sequence(vec![("200 OK", candidates), ("200 OK", albums)]).await;
        let client = lidarr_client(url, true)
            .with_musicbrainz_disc_release_lookup(Arc::new(FakeMusicBrainzLookup {
                result: MusicBrainzDiscLookupResult::Found {
                    releases: vec![musicbrainz_release(
                        "cccccccc-cccc-cccc-cccc-cccccccccccc",
                        16,
                    )],
                    diagnostic: "MusicBrainz lookup: found 1 release(s)\n".into(),
                },
            }))
            .with_disc_release_lookup(Arc::new(FakeDiscLookup {
                result: DiscReleaseLookupResult::NotFound {
                    diagnostic: "GnuDB lookup: no search candidates\n".into(),
                },
            }));

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata_and_disc_id(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1984",
                Some("C60C9D10"),
                vec![("/downloads/album/Artist - Album - 01 - One.flac", "One")],
            ))
            .await
            .unwrap();

        let ManualImportResult::Skipped { diagnostic, .. } = result else {
            panic!("expected manual import to skip");
        };
        assert!(diagnostic.contains("MusicBrainz exact intersection count: 0"));
        assert!(diagnostic.contains("MusicBrainz best-effort candidate count: 0"));
        assert!(diagnostic.contains(
            "MusicBrainz decision: no compatible Lidarr release, falling through to GnuDB"
        ));
        assert!(diagnostic.contains("Fallback MusicBrainz add missing release group: false"));
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn manual_import_fallback_adds_missing_musicbrainz_release_group_and_imports() {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/Artist - Album - 02 - Two.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let initial_albums = "[]";
        let search = r#"[
            {"album":{"id":0,"title":"Album","foreignAlbumId":"11111111-1111-1111-1111-111111111111","releases":[]}}
        ]"#;
        let added_album =
            r#"{"id":4,"title":"Album","foreignAlbumId":"11111111-1111-1111-1111-111111111111"}"#;
        let refetched_albums = r#"[
            {"id":4,"title":"Album","artistId":1,"foreignAlbumId":"11111111-1111-1111-1111-111111111111","releaseDate":"1984-01-01","releases":[
                {"id":5,"albumId":4,"foreignReleaseId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Album","trackCount":2,"monitored":false,"format":"CD"}
            ]}
        ]"#;
        let tracks = r#"[
            {"id":11,"albumId":4,"absoluteTrackNumber":1,"trackNumber":"1","title":"One"},
            {"id":12,"albumId":4,"absoluteTrackNumber":2,"trackNumber":"2","title":"Two"}
        ]"#;
        let (url, requests) = serve_sequence(vec![
            ("200 OK", candidates),
            ("200 OK", initial_albums),
            ("200 OK", search),
            ("201 Created", added_album),
            ("200 OK", refetched_albums),
            ("200 OK", tracks),
            ("201 Created", r#"{"id":7}"#),
        ])
        .await;
        let client = lidarr_client(url, true)
            .with_musicbrainz_add_missing_release_group(true)
            .with_musicbrainz_add_album_refetch(1, Duration::ZERO)
            .with_musicbrainz_disc_release_lookup(Arc::new(FakeMusicBrainzLookup {
                result: MusicBrainzDiscLookupResult::Found {
                    releases: vec![musicbrainz_release(
                        "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                        2,
                    )],
                    diagnostic: "MusicBrainz lookup: found 1 release(s)\n".into(),
                },
            }));

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata_and_disc_id(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1984",
                Some("C60C9D10"),
                vec![
                    ("/downloads/album/Artist - Album - 01 - One.flac", "One"),
                    ("/downloads/album/Artist - Album - 02 - Two.flac", "Two"),
                ],
            ))
            .await
            .unwrap();

        let ManualImportResult::Started { diagnostic, .. } = result else {
            panic!("expected manual import to start");
        };
        assert!(diagnostic.contains("Fallback MusicBrainz add missing release group: true"));
        assert!(diagnostic.contains(
            "MusicBrainz add missing release group distinct release_group_ids=[11111111-1111-1111-1111-111111111111]"
        ));
        assert!(diagnostic.contains(
            "MusicBrainz add missing release group decision: Lidarr search album accepted"
        ));
        assert!(diagnostic.contains(
            "MusicBrainz add missing release group decision: added release-group selected"
        ));
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 7);
        assert!(requests[2].starts_with("GET /api/v1/search?"));
        assert!(requests[2].contains("lidarr%3A11111111-1111-1111-1111-111111111111"));
        assert!(requests[3].starts_with("POST /api/v1/album "));
        let add_album: Value = serde_json::from_str(request_body(&requests[3])).unwrap();
        assert_eq!(add_album["artistId"], 1);
        assert_eq!(add_album["artist"]["id"], 1);
        assert_eq!(add_album["artist"]["artistName"], "Artist");
        assert_eq!(add_album["monitored"], false);
        assert_eq!(add_album["addOptions"]["addType"], "manual");
        assert_eq!(add_album["addOptions"]["searchForNewAlbum"], false);
        assert!(requests[4].contains("artistId=1"));
        assert!(requests[5].contains("albumId=4"));
        let command: Value = serde_json::from_str(request_body(&requests[6])).unwrap();
        assert_eq!(command["files"][0]["albumId"], 4);
        assert_eq!(command["files"][0]["albumReleaseId"], 5);
        assert_eq!(command["files"][0]["trackIds"], serde_json::json!([11]));
        assert_eq!(command["files"][1]["trackIds"], serde_json::json!([12]));
    }

    #[tokio::test]
    async fn manual_import_fallback_adds_missing_musicbrainz_release_group_before_gnudb() {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/Artist - Album - 02 - Two.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/Artist - Album - 03 - Three.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let initial_albums = r#"[
            {"id":2,"title":"Album","artistId":1,"artist":{"id":1,"artistName":"Artist","foreignArtistId":"artist-mbid","qualityProfileId":7,"metadataProfileId":3,"rootFolderPath":"/music"},"releaseDate":"1984-01-01","releases":[
                {"id":3,"albumId":2,"foreignReleaseId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Album","trackCount":2,"monitored":true,"format":"7\" Vinyl"},
                {"id":4,"albumId":2,"foreignReleaseId":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb","title":"Album","trackCount":2,"monitored":false,"format":"12\" Vinyl"}
            ]}
        ]"#;
        let search = r#"[
            {"album":{"id":0,"title":"Album: 3 Happy Songs","foreignAlbumId":"11111111-1111-1111-1111-111111111111","releases":[
                {"id":99,"albumId":0,"foreignReleaseId":"cccccccc-cccc-cccc-cccc-cccccccccccc","title":"Album: 3 Happy Songs","trackCount":3,"monitored":false,"format":"CD","media":[{"mediumNumber":1}]}
            ]}}
        ]"#;
        let added_album = r#"{"id":6,"title":"Album: 3 Happy Songs","foreignAlbumId":"11111111-1111-1111-1111-111111111111"}"#;
        let refetched_albums_without_releases = r#"[
            {"id":2,"title":"Album","artistId":1,"releaseDate":"1984-01-01","releases":[
                {"id":3,"albumId":2,"foreignReleaseId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Album","trackCount":2,"monitored":true,"format":"7\" Vinyl"}
            ]},
            {"id":6,"title":"Album: 3 Happy Songs","artistId":1,"foreignAlbumId":"11111111-1111-1111-1111-111111111111","releaseDate":"1984-01-01","releases":[]}
        ]"#;
        let refetched_albums = r#"[
            {"id":2,"title":"Album","artistId":1,"releaseDate":"1984-01-01","releases":[
                {"id":3,"albumId":2,"foreignReleaseId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Album","trackCount":2,"monitored":true,"format":"7\" Vinyl"}
            ]},
            {"id":6,"title":"Album: 3 Happy Songs","artistId":1,"foreignAlbumId":"11111111-1111-1111-1111-111111111111","releaseDate":"1984-01-01","releases":[
                {"id":7,"albumId":6,"foreignReleaseId":"cccccccc-cccc-cccc-cccc-cccccccccccc","title":"Album: 3 Happy Songs","trackCount":3,"monitored":false,"format":"CD"}
            ]}
        ]"#;
        let tracks = r#"[
            {"id":11,"albumId":6,"absoluteTrackNumber":1,"trackNumber":"1","title":"One"},
            {"id":12,"albumId":6,"absoluteTrackNumber":2,"trackNumber":"2","title":"Two"},
            {"id":13,"albumId":6,"absoluteTrackNumber":3,"trackNumber":"3","title":"Three"}
        ]"#;
        let (url, requests) = serve_sequence(vec![
            ("200 OK", candidates),
            ("200 OK", initial_albums),
            ("200 OK", search),
            ("201 Created", added_album),
            ("200 OK", refetched_albums_without_releases),
            ("200 OK", refetched_albums),
            ("200 OK", tracks),
            ("201 Created", r#"{"id":7}"#),
        ])
        .await;
        let client = lidarr_client(url, true)
            .with_musicbrainz_add_missing_release_group(true)
            .with_musicbrainz_add_album_refetch(5, Duration::ZERO)
            .with_musicbrainz_disc_release_lookup(Arc::new(FakeMusicBrainzLookup {
                result: MusicBrainzDiscLookupResult::Found {
                    releases: vec![musicbrainz_release_with_title(
                        "cccccccc-cccc-cccc-cccc-cccccccccccc",
                        "Album: 3 Happy Songs",
                        3,
                    )],
                    diagnostic: "MusicBrainz lookup: found 1 release(s)\n".into(),
                },
            }));

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata_and_disc_id(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1984",
                Some("C60C9D10"),
                vec![
                    ("/downloads/album/Artist - Album - 01 - One.flac", "One"),
                    ("/downloads/album/Artist - Album - 02 - Two.flac", "Two"),
                    ("/downloads/album/Artist - Album - 03 - Three.flac", "Three"),
                ],
            ))
            .await
            .unwrap();

        let ManualImportResult::Started { diagnostic, .. } = result else {
            panic!("expected manual import to start");
        };
        assert!(diagnostic.contains("MusicBrainz best-effort candidate count: 0"));
        assert!(diagnostic.contains("Fallback MusicBrainz add missing release group: true"));
        assert!(diagnostic
            .contains("MusicBrainz add missing release group search: path=/api/v1/search"));
        assert!(
            !diagnostic.contains("Fallback GnuDB release lookup: evaluating"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains(
            "prepared Lidarr add album payload artist_source=lidarr-album stripped_search_releases=1"
        ));
        assert!(diagnostic
            .contains("MusicBrainz add missing release group refetch attempt=1: album_id=6"));
        assert!(diagnostic.contains("matching_releases=0"));
        assert!(diagnostic
            .contains("MusicBrainz add missing release group refetch attempt=2: album_id=6"));
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 8);
        assert!(requests[2].starts_with("GET /api/v1/search?"));
        let add_album: Value = serde_json::from_str(request_body(&requests[3])).unwrap();
        assert_eq!(add_album["artistId"], 1);
        assert_eq!(add_album["artist"]["id"], 1);
        assert_eq!(add_album["artist"]["artistName"], "Artist");
        assert_eq!(add_album["artist"]["foreignArtistId"], "artist-mbid");
        assert_eq!(add_album["artist"]["qualityProfileId"], 7);
        assert_eq!(add_album["artist"]["metadataProfileId"], 3);
        assert_eq!(add_album["artist"]["rootFolderPath"], "/music");
        assert_eq!(add_album["monitored"], false);
        assert_eq!(add_album["addOptions"]["searchForNewAlbum"], false);
        assert!(add_album.get("releases").is_none());
        assert!(requests[6].contains("albumId=6"));
        let command: Value = serde_json::from_str(request_body(&requests[7])).unwrap();
        assert_eq!(command["files"][0]["albumId"], 6);
        assert_eq!(command["files"][0]["albumReleaseId"], 7);
        assert_eq!(command["files"][2]["trackIds"], serde_json::json!([13]));
    }

    #[tokio::test]
    async fn manual_import_fallback_add_missing_musicbrainz_release_group_does_not_retry_after_add()
    {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/Artist - Album - 02 - Two.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/Artist - Album - 03 - Three.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let initial_albums = r#"[
            {"id":2,"title":"Album","artistId":1,"artist":{"id":1,"artistName":"Artist","foreignArtistId":"artist-mbid","qualityProfileId":7,"metadataProfileId":3,"rootFolderPath":"/music"},"releaseDate":"1984-01-01","releases":[
                {"id":3,"albumId":2,"foreignReleaseId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Album","trackCount":2,"monitored":true,"format":"7\" Vinyl"},
                {"id":4,"albumId":2,"foreignReleaseId":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb","title":"Album","trackCount":2,"monitored":false,"format":"12\" Vinyl"}
            ]}
        ]"#;
        let search = r#"[
            {"album":{"id":0,"title":"Album: 3 Happy Songs","foreignAlbumId":"11111111-1111-1111-1111-111111111111","releases":[
                {"id":99,"albumId":0,"foreignReleaseId":"cccccccc-cccc-cccc-cccc-cccccccccccc","title":"Album: 3 Happy Songs","trackCount":3,"monitored":false,"format":"CD","media":[{"mediumNumber":1}]}
            ]}}
        ]"#;
        let added_album = r#"{"id":6,"title":"Album: 3 Happy Songs","foreignAlbumId":"11111111-1111-1111-1111-111111111111"}"#;
        let refetched_albums = r#"[
            {"id":2,"title":"Album","artistId":1,"releaseDate":"1984-01-01","releases":[
                {"id":3,"albumId":2,"foreignReleaseId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Album","trackCount":2,"monitored":true,"format":"7\" Vinyl"}
            ]},
            {"id":6,"title":"Album: 3 Happy Songs","artistId":1,"foreignAlbumId":"11111111-1111-1111-1111-111111111111","releaseDate":"1984-01-01","releases":[
                {"id":7,"albumId":6,"foreignReleaseId":"cccccccc-cccc-cccc-cccc-cccccccccccc","title":"Album: 3 Happy Songs","trackCount":2,"monitored":false,"format":"CD"}
            ]}
        ]"#;
        let (url, requests) = serve_sequence(vec![
            ("200 OK", candidates),
            ("200 OK", initial_albums),
            ("200 OK", search),
            ("201 Created", added_album),
            ("200 OK", refetched_albums),
        ])
        .await;
        let client = lidarr_client(url, true)
            .with_musicbrainz_add_missing_release_group(true)
            .with_musicbrainz_add_album_refetch(1, Duration::ZERO)
            .with_musicbrainz_disc_release_lookup(Arc::new(FakeMusicBrainzLookup {
                result: MusicBrainzDiscLookupResult::Found {
                    releases: vec![musicbrainz_release_with_title(
                        "cccccccc-cccc-cccc-cccc-cccccccccccc",
                        "Album: 3 Happy Songs",
                        3,
                    )],
                    diagnostic: "MusicBrainz lookup: found 1 release(s)\n".into(),
                },
            }))
            .with_disc_release_lookup(Arc::new(FakeDiscLookup {
                result: DiscReleaseLookupResult::NotFound {
                    diagnostic: "GnuDB lookup: no search candidates\n".into(),
                },
            }));

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata_and_disc_id(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1984",
                Some("C60C9D10"),
                vec![
                    ("/downloads/album/Artist - Album - 01 - One.flac", "One"),
                    ("/downloads/album/Artist - Album - 02 - Two.flac", "Two"),
                    ("/downloads/album/Artist - Album - 03 - Three.flac", "Three"),
                ],
            ))
            .await
            .unwrap();

        let ManualImportResult::Skipped { diagnostic, .. } = result else {
            panic!("expected manual import to skip");
        };
        assert!(diagnostic.contains(
            "MusicBrainz add missing release group decision: added album has no compatible release"
        ));
        assert!(diagnostic.contains("Fallback GnuDB release lookup: evaluating"));
        let requests = requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /api/v1/album "))
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("GET /api/v1/search?"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn manual_import_fallback_add_missing_musicbrainz_release_group_skips_ambiguous_groups() {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let (url, requests) = serve_sequence(vec![("200 OK", candidates), ("200 OK", "[]")]).await;
        let mut first = musicbrainz_release("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", 1);
        first.release_group_id = Some("11111111-1111-1111-1111-111111111111".into());
        let mut second = musicbrainz_release("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", 1);
        second.release_group_id = Some("22222222-2222-2222-2222-222222222222".into());
        let client = lidarr_client(url, true)
            .with_musicbrainz_add_missing_release_group(true)
            .with_musicbrainz_disc_release_lookup(Arc::new(FakeMusicBrainzLookup {
                result: MusicBrainzDiscLookupResult::Found {
                    releases: vec![first, second],
                    diagnostic: "MusicBrainz lookup: found 2 release(s)\n".into(),
                },
            }));

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata_and_disc_id(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1984",
                Some("C60C9D10"),
                vec![("/downloads/album/Artist - Album - 01 - One.flac", "One")],
            ))
            .await
            .unwrap();

        let ManualImportResult::Skipped { diagnostic, .. } = result else {
            panic!("expected manual import to skip");
        };
        assert!(diagnostic.contains("Fallback MusicBrainz add missing release group: true"));
        assert!(diagnostic.contains("expected one distinct release-group ID, got 2"));
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn manual_import_fallback_trusted_musicbrainz_disabled_preserves_ambiguous_album_skip() {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Kalimba De Luna - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/Artist - Kalimba De Luna - 02 - Two.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/Artist - Kalimba De Luna - 03 - Three.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let albums = r#"[
            {"id":2,"title":"Kalimba de luna","artistId":1,"releaseDate":"1984-01-01","releases":[
                {"id":3,"albumId":2,"foreignReleaseId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Kalimba de luna","trackCount":2,"monitored":true,"format":"7\" Vinyl"}
            ]},
            {"id":4,"title":"Kalimba de Luna: 3 Happy Songs","artistId":1,"releaseDate":"1984-01-01","releases":[
                {"id":5,"albumId":4,"foreignReleaseId":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb","title":"Kalimba de Luna: 3 Happy Songs","trackCount":3,"monitored":true,"format":"CD"}
            ]}
        ]"#;
        let (url, requests) =
            serve_sequence(vec![("200 OK", candidates), ("200 OK", albums)]).await;
        let client = lidarr_client(url, true).with_musicbrainz_disc_release_lookup(Arc::new(
            FakeMusicBrainzLookup {
                result: MusicBrainzDiscLookupResult::Found {
                    releases: vec![musicbrainz_release_with_title(
                        "cccccccc-cccc-cccc-cccc-cccccccccccc",
                        "Kalimba de Luna: 3 Happy Songs",
                        3,
                    )],
                    diagnostic: "MusicBrainz lookup should not run\n".into(),
                },
            },
        ));

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata_and_disc_id(
                "Artist - Kalimba De Luna",
                "/downloads/album",
                "Kalimba De Luna",
                "Artist",
                "1984",
                Some("C60C9D10"),
                vec![
                    (
                        "/downloads/album/Artist - Kalimba De Luna - 01 - One.flac",
                        "One",
                    ),
                    (
                        "/downloads/album/Artist - Kalimba De Luna - 02 - Two.flac",
                        "Two",
                    ),
                    (
                        "/downloads/album/Artist - Kalimba De Luna - 03 - Three.flac",
                        "Three",
                    ),
                ],
            ))
            .await
            .unwrap();

        let ManualImportResult::Skipped { reason, diagnostic } = result else {
            panic!("expected manual import to skip");
        };
        assert!(reason.contains("multiple Lidarr albums matched fallback hints"));
        assert!(!diagnostic.contains("MusicBrainz lookup should not run"));
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn manual_import_fallback_trusted_musicbrainz_widens_to_matching_album_title() {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Kalimba De Luna - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/Artist - Kalimba De Luna - 02 - Two.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/Artist - Kalimba De Luna - 03 - Three.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let albums = r#"[
            {"id":2,"title":"Kalimba de luna","artistId":1,"releaseDate":"1984-01-01","releases":[
                {"id":3,"albumId":2,"foreignReleaseId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Kalimba de luna","trackCount":2,"monitored":true,"format":"7\" Vinyl"}
            ]},
            {"id":4,"title":"Kalimba de Luna: 3 Happy Songs","artistId":1,"releaseDate":"1984-01-01","releases":[
                {"id":5,"albumId":4,"foreignReleaseId":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb","title":"Kalimba de Luna: 3 Happy Songs","trackCount":3,"monitored":true,"format":"CD"}
            ]}
        ]"#;
        let tracks = r#"[
            {"id":11,"albumId":4,"absoluteTrackNumber":1,"trackNumber":"1","title":"One"},
            {"id":12,"albumId":4,"absoluteTrackNumber":2,"trackNumber":"2","title":"Two"},
            {"id":13,"albumId":4,"absoluteTrackNumber":3,"trackNumber":"3","title":"Three"}
        ]"#;
        let (url, requests) = serve_sequence(vec![
            ("200 OK", candidates),
            ("200 OK", albums),
            ("200 OK", tracks),
            ("201 Created", r#"{"id":7}"#),
        ])
        .await;
        let client = lidarr_client(url, true)
            .with_musicbrainz_trust_disc_lookup(true)
            .with_musicbrainz_disc_release_lookup(Arc::new(FakeMusicBrainzLookup {
                result: MusicBrainzDiscLookupResult::Found {
                    releases: vec![
                        musicbrainz_release_with_title(
                            "cccccccc-cccc-cccc-cccc-cccccccccccc",
                            "Kalimba de Luna: 3 Happy Songs",
                            3,
                        ),
                        {
                            let mut release = musicbrainz_release_with_title(
                                "dddddddd-dddd-dddd-dddd-dddddddddddd",
                                "Kalimba de Luna: 3 Happy Songs",
                                3,
                            );
                            release.barcode = None;
                            release
                        },
                    ],
                    diagnostic: "MusicBrainz lookup: found 2 release(s)\n".into(),
                },
            }));

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata_and_disc_id(
                "Artist - Kalimba De Luna",
                "/downloads/album",
                "Kalimba De Luna",
                "Artist",
                "1984",
                Some("C60C9D10"),
                vec![
                    (
                        "/downloads/album/Artist - Kalimba De Luna - 01 - One.flac",
                        "One",
                    ),
                    (
                        "/downloads/album/Artist - Kalimba De Luna - 02 - Two.flac",
                        "Two",
                    ),
                    (
                        "/downloads/album/Artist - Kalimba De Luna - 03 - Three.flac",
                        "Three",
                    ),
                ],
            ))
            .await
            .unwrap();

        let ManualImportResult::Started { diagnostic, .. } = result else {
            panic!("expected manual import to start");
        };
        assert!(diagnostic.contains("Fallback album match was ambiguous"));
        assert!(diagnostic.contains("MusicBrainz trusted titles: [kalimba de luna 3 happy songs]"));
        assert!(diagnostic.contains("MusicBrainz trusted decision: widened album/release selected"));
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[2].contains("albumId=4"));
        let command: Value = serde_json::from_str(request_body(&requests[3])).unwrap();
        assert_eq!(command["files"][0]["albumId"], 4);
        assert_eq!(command["files"][0]["albumReleaseId"], 5);
        assert_eq!(command["files"][0]["trackIds"], serde_json::json!([11]));
        assert_eq!(command["files"][2]["trackIds"], serde_json::json!([13]));
    }

    #[tokio::test]
    async fn manual_import_fallback_ambiguous_musicbrainz_releases_fall_through_to_gnudb() {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}},
            {"path":"/downloads/album/Artist - Album - 02 - Two.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let albums = r#"[
            {"id":2,"title":"Album","artistId":1,"releaseDate":"1984-01-01","releases":[
                {"id":3,"albumId":2,"foreignReleaseId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Album","trackCount":2,"monitored":false},
                {"id":4,"albumId":2,"foreignReleaseId":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb","title":"Album","trackCount":2,"monitored":false}
            ]}
        ]"#;
        let tracks = r#"[
            {"id":11,"albumId":2,"absoluteTrackNumber":1,"trackNumber":"1","title":"One"},
            {"id":12,"albumId":2,"absoluteTrackNumber":2,"trackNumber":"2","title":"Two"}
        ]"#;
        let (url, requests) = serve_sequence(vec![
            ("200 OK", candidates),
            ("200 OK", albums),
            ("200 OK", tracks),
            ("201 Created", r#"{"id":7}"#),
        ])
        .await;
        let client = lidarr_client(url, true)
            .with_musicbrainz_disc_release_lookup(Arc::new(FakeMusicBrainzLookup {
                result: MusicBrainzDiscLookupResult::Found {
                    releases: vec![
                        MusicBrainzDiscRelease {
                            id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into(),
                            title: Some("Album".into()),
                            media_count: 1,
                            media_track_counts: vec![2],
                            ..MusicBrainzDiscRelease::default()
                        },
                        MusicBrainzDiscRelease {
                            id: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".into(),
                            title: Some("Album".into()),
                            media_count: 1,
                            media_track_counts: vec![2],
                            ..MusicBrainzDiscRelease::default()
                        },
                    ],
                    diagnostic: "MusicBrainz lookup: found 2 release(s)\n".into(),
                },
            }))
            .with_disc_release_lookup(Arc::new(FakeDiscLookup {
                result: DiscReleaseLookupResult::Found {
                    candidates: vec![DiscReleaseCandidate {
                        category: "rock".into(),
                        entry_id: "c60c9d10".into(),
                        disc_id: "C60C9D10".into(),
                        artist: Some("Artist".into()),
                        title: Some("Album".into()),
                        year: Some(1984),
                        track_titles: vec!["One".into(), "Two".into()],
                        art_ids: vec!["bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".into()],
                    }],
                    diagnostic: "GnuDB accepted candidates: 1\n".into(),
                },
            }));

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata_and_disc_id(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1984",
                Some("C60C9D10"),
                vec![
                    ("/downloads/album/Artist - Album - 01 - One.flac", "One"),
                    ("/downloads/album/Artist - Album - 02 - Two.flac", "Two"),
                ],
            ))
            .await
            .unwrap();

        let ManualImportResult::Started { diagnostic, .. } = result else {
            panic!("expected manual import to start");
        };
        assert!(diagnostic.contains("MusicBrainz exact intersection count: 2"));
        assert!(diagnostic.contains("MusicBrainz decision: ranked candidates tied"));
        assert!(diagnostic
            .contains("GnuDB selected foreign_release_id=bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"));
        assert_eq!(requests.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn manual_import_fallback_skips_when_gnudb_artids_do_not_match_lidarr_releases() {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let albums = r#"[
            {"id":2,"title":"Album","artistId":1,"releaseDate":"1984-01-01","releases":[
                {"id":3,"albumId":2,"foreignReleaseId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Album","trackCount":1},
                {"id":4,"albumId":2,"foreignReleaseId":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb","title":"Album","trackCount":1}
            ]}
        ]"#;
        let (url, requests) =
            serve_sequence(vec![("200 OK", candidates), ("200 OK", albums)]).await;
        let client = lidarr_client(url, true).with_disc_release_lookup(Arc::new(FakeDiscLookup {
            result: DiscReleaseLookupResult::Found {
                candidates: vec![DiscReleaseCandidate {
                    category: "rock".into(),
                    entry_id: "c60c9d10".into(),
                    disc_id: "C60C9D10".into(),
                    artist: Some("Artist".into()),
                    title: Some("Album".into()),
                    year: Some(1984),
                    track_titles: vec!["One".into()],
                    art_ids: vec!["cccccccc-cccc-cccc-cccc-cccccccccccc".into()],
                }],
                diagnostic: "GnuDB accepted candidates: 1\n".into(),
            },
        }));

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata_and_disc_id(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1984",
                Some("C60C9D10"),
                vec![("/downloads/album/Artist - Album - 01 - One.flac", "One")],
            ))
            .await
            .unwrap();

        let ManualImportResult::Skipped { reason, diagnostic } = result else {
            panic!("expected manual import to skip");
        };
        assert!(reason.contains("multiple Lidarr album releases"));
        assert!(diagnostic.contains("matches=0"));
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn manual_import_fallback_uses_gnudb_when_release_track_count_does_not_match() {
        let candidates = r#"[
            {"path":"/downloads/album/Artist - Album - 01 - One.flac","artist":{"id":1,"artistName":"Artist"},"quality":{"quality":{"id":6}}}
        ]"#;
        let albums = r#"[
            {"id":2,"title":"Album","artistId":1,"releaseDate":"1984-01-01","releases":[
                {"id":3,"albumId":2,"foreignReleaseId":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"Album","trackCount":2}
            ]}
        ]"#;
        let tracks = r#"[
            {"id":11,"albumId":2,"absoluteTrackNumber":1,"trackNumber":"1","title":"One"}
        ]"#;
        let (url, requests) = serve_sequence(vec![
            ("200 OK", candidates),
            ("200 OK", albums),
            ("200 OK", tracks),
            ("201 Created", r#"{"id":7}"#),
        ])
        .await;
        let client = lidarr_client(url, true).with_disc_release_lookup(Arc::new(FakeDiscLookup {
            result: DiscReleaseLookupResult::Found {
                candidates: vec![DiscReleaseCandidate {
                    category: "rock".into(),
                    entry_id: "c60c9d10".into(),
                    disc_id: "C60C9D10".into(),
                    artist: Some("Artist".into()),
                    title: Some("Album".into()),
                    year: Some(1984),
                    track_titles: vec!["One".into()],
                    art_ids: vec!["aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into()],
                }],
                diagnostic: "GnuDB accepted candidates: 1\n".into(),
            },
        }));

        let result = client
            .trigger_manual_import(manual_import_request_with_metadata_and_disc_id(
                "Artist - Album",
                "/downloads/album",
                "Album",
                "Artist",
                "1984",
                Some("C60C9D10"),
                vec![("/downloads/album/Artist - Album - 01 - One.flac", "One")],
            ))
            .await
            .unwrap();

        let ManualImportResult::Started { diagnostic, .. } = result else {
            panic!("expected manual import to start");
        };
        assert!(diagnostic.contains("matching_releases=0"));
        assert!(diagnostic
            .contains("GnuDB selected foreign_release_id=aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"));
        assert_eq!(requests.lock().unwrap().len(), 4);
    }

    fn lidarr_client(url: String, manual_import_enabled: bool) -> LidarrQueueSource {
        LidarrQueueSource::new(&LidarrSettings {
            url,
            api_key: "secret".to_owned(),
            queue_page_size: 100,
            queue_max_pages: 100,
            manual_import_enabled,
        })
    }

    fn manual_import_request(paths: Vec<&str>, track_count: usize) -> ManualImportRequest {
        ManualImportRequest {
            download: TrackedDownload::pending(
                "download-1".into(),
                "Artist - Album".into(),
                "completed".into(),
                "/downloads/album".into(),
                "importFailed".into(),
            ),
            generated_tracks: paths.into_iter().map(PathBuf::from).collect(),
            cue_hints: vec![CueMetadataHint {
                path: PathBuf::from("/downloads/album/album.cue"),
                album_title: Some("Album".into()),
                performer: Some("Artist".into()),
                catalog: None,
                disc_id: None,
                comments: Vec::new(),
                track_count,
                tracks: (1..=track_count)
                    .map(|number| CueTrackHint {
                        number: number.to_string(),
                        title: None,
                        performer: Some("Artist".into()),
                    })
                    .collect(),
            }],
        }
    }

    fn manual_import_request_with_metadata(
        title: &str,
        output_path: &str,
        album_title: &str,
        performer: &str,
        year: &str,
        tracks: Vec<(&str, &str)>,
    ) -> ManualImportRequest {
        manual_import_request_with_metadata_and_disc_id(
            title,
            output_path,
            album_title,
            performer,
            year,
            None,
            tracks,
        )
    }

    fn manual_import_request_with_metadata_and_disc_id(
        title: &str,
        output_path: &str,
        album_title: &str,
        performer: &str,
        year: &str,
        disc_id: Option<&str>,
        tracks: Vec<(&str, &str)>,
    ) -> ManualImportRequest {
        ManualImportRequest {
            download: TrackedDownload::pending(
                "download-1".into(),
                title.into(),
                "completed".into(),
                output_path.into(),
                "importFailed".into(),
            ),
            generated_tracks: tracks.iter().map(|(path, _)| PathBuf::from(path)).collect(),
            cue_hints: vec![CueMetadataHint {
                path: PathBuf::from(format!("{output_path}/album.cue")),
                album_title: Some(album_title.into()),
                performer: Some(performer.into()),
                catalog: None,
                disc_id: disc_id.map(str::to_owned),
                comments: vec![("DATE".into(), year.into())],
                track_count: tracks.len(),
                tracks: tracks
                    .iter()
                    .enumerate()
                    .map(|(index, (_, title))| CueTrackHint {
                        number: (index + 1).to_string(),
                        title: Some((*title).into()),
                        performer: Some(performer.into()),
                    })
                    .collect(),
            }],
        }
    }

    struct FakeDiscLookup {
        result: DiscReleaseLookupResult,
    }

    #[async_trait::async_trait]
    impl DiscReleaseLookup for FakeDiscLookup {
        async fn lookup_disc_release(
            &self,
            _request: DiscReleaseLookupRequest,
        ) -> anyhow::Result<DiscReleaseLookupResult> {
            Ok(self.result.clone())
        }
    }

    struct FakeMusicBrainzLookup {
        result: MusicBrainzDiscLookupResult,
    }

    #[async_trait::async_trait]
    impl MusicBrainzDiscReleaseLookup for FakeMusicBrainzLookup {
        async fn lookup_musicbrainz_disc_releases(
            &self,
            _request: MusicBrainzDiscLookupRequest,
        ) -> anyhow::Result<MusicBrainzDiscLookupResult> {
            Ok(self.result.clone())
        }
    }

    fn musicbrainz_release(id: &str, track_count: usize) -> MusicBrainzDiscRelease {
        musicbrainz_release_with_title(id, "Album", track_count)
    }

    fn musicbrainz_release_with_title(
        id: &str,
        title: &str,
        track_count: usize,
    ) -> MusicBrainzDiscRelease {
        MusicBrainzDiscRelease {
            id: id.into(),
            title: Some(title.into()),
            date: Some("1984-01-01".into()),
            country: Some("DE".into()),
            status: Some("Official".into()),
            barcode: Some("1234567890123".into()),
            quality: Some("normal".into()),
            media_count: 1,
            media_formats: vec!["CD".into()],
            media_track_counts: vec![track_count],
            label_count: 1,
            release_group_id: Some("11111111-1111-1111-1111-111111111111".into()),
            release_group_title: Some(title.into()),
            release_group_first_release_date: Some("1984".into()),
        }
    }

    fn request_body(request: &str) -> &str {
        request.split("\r\n\r\n").nth(1).unwrap_or_default()
    }

    async fn serve_once(status: &'static str, body: &'static str) -> String {
        let (url, _) = serve_sequence(vec![(status, body)]).await;
        url
    }

    async fn serve_sequence(
        responses: Vec<(&'static str, &'static str)>,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let shared_requests = Arc::clone(&requests);
        let shared_responses = Arc::new(Mutex::new(
            responses
                .into_iter()
                .collect::<VecDeque<(&'static str, &'static str)>>(),
        ));
        let queue = Arc::clone(&shared_responses);

        tokio::spawn(async move {
            loop {
                let next = { queue.lock().unwrap().pop_front() };
                let Some((status, body)) = next else {
                    break;
                };

                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 4096];
                let bytes_read = socket.read(&mut request).await.unwrap();
                let request_text = String::from_utf8_lossy(&request[..bytes_read]);
                shared_requests
                    .lock()
                    .unwrap()
                    .push(request_text.into_owned());

                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });

        (format!("http://{addr}"), requests)
    }
}
