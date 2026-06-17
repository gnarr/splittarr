use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::Client;

use crate::application::ports::{
    DiscReleaseCandidate, DiscReleaseLookup, DiscReleaseLookupRequest, DiscReleaseLookupResult,
};
use crate::bootstrap::settings::GnudbSettings;

#[derive(Debug, Clone)]
pub struct GnudbDiscReleaseLookup {
    enabled: bool,
    server: String,
    user_email: String,
    client: Client,
}

impl GnudbDiscReleaseLookup {
    pub fn new(settings: &GnudbSettings) -> Self {
        Self {
            enabled: settings.disc_lookup_enabled,
            server: settings.server.clone(),
            user_email: settings.user_email.clone(),
            client: Client::new(),
        }
    }

    async fn request(&self, command: String) -> Result<String> {
        let response = self
            .client
            .get(self.endpoint_url())
            .query(&[
                ("cmd", command.as_str()),
                ("hello", self.hello().as_str()),
                ("proto", "6"),
            ])
            .send()
            .await
            .map_err(|err| anyhow!("failed requesting GnuDB: {err}"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| anyhow!("failed reading GnuDB response: {err}"))?;

        if !status.is_success() {
            return Err(anyhow!("GnuDB returned HTTP {status}: {body}"));
        }

        Ok(body)
    }

    fn hello(&self) -> String {
        let (name, host) = self
            .user_email
            .split_once('@')
            .unwrap_or(("splittarr", "localhost"));
        format!("{}+{}+splittarr+{}", name, host, env!("CARGO_PKG_VERSION"))
    }

    fn endpoint_url(&self) -> String {
        if self.server.starts_with("http://") || self.server.starts_with("https://") {
            return self.server.clone();
        }
        format!("http://{}/~cddb/cddb.cgi", self.server)
    }
}

#[async_trait]
impl DiscReleaseLookup for GnudbDiscReleaseLookup {
    async fn lookup_disc_release(
        &self,
        request: DiscReleaseLookupRequest,
    ) -> Result<DiscReleaseLookupResult> {
        if !self.enabled {
            return Ok(DiscReleaseLookupResult::Disabled {
                diagnostic: "GnuDB lookup: disabled\n".into(),
            });
        }

        let mut diagnostic = String::new();
        diagnostic.push_str("GnuDB lookup: enabled\n");
        diagnostic.push_str(&format!("GnuDB requested DISCID: {}\n", request.disc_id));
        diagnostic.push_str(&format!(
            "GnuDB search inputs: artist={} album={} year={} tracks={}\n",
            request.artist.as_deref().unwrap_or("-"),
            request.album_title.as_deref().unwrap_or("-"),
            request
                .year
                .map(|year| year.to_string())
                .unwrap_or_else(|| "-".into()),
            request.track_count
        ));

        let Some(artist) = request
            .artist
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        else {
            diagnostic.push_str("GnuDB lookup: skipped because artist hint is missing\n");
            return Ok(DiscReleaseLookupResult::NotFound { diagnostic });
        };
        let Some(album) = request
            .album_title
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        else {
            diagnostic.push_str("GnuDB lookup: skipped because album hint is missing\n");
            return Ok(DiscReleaseLookupResult::NotFound { diagnostic });
        };

        let search_command = format!(
            "search artist {artist} album {album} tracks {}",
            request.track_count
        );
        let search_body = match self.request(search_command).await {
            Ok(body) => body,
            Err(err) => {
                diagnostic.push_str(&format!("GnuDB lookup failed: {err}\n"));
                return Ok(DiscReleaseLookupResult::NotFound { diagnostic });
            }
        };
        let search_candidates = parse_search_response(&search_body);
        diagnostic.push_str(&format!(
            "GnuDB search candidates: {}\n",
            search_candidates.len()
        ));

        if search_candidates.is_empty() {
            diagnostic.push_str("GnuDB lookup: no search candidates\n");
            return Ok(DiscReleaseLookupResult::NotFound { diagnostic });
        }

        let mut accepted = Vec::new();
        for search_candidate in search_candidates {
            diagnostic.push_str(&format!(
                "GnuDB read candidate: category={} id={} title={}\n",
                search_candidate.category, search_candidate.entry_id, search_candidate.title
            ));
            let read_body = match self
                .request(format!(
                    "cddb read {} {}",
                    search_candidate.category, search_candidate.entry_id
                ))
                .await
            {
                Ok(body) => body,
                Err(err) => {
                    diagnostic.push_str(&format!("  read failed: {err}\n"));
                    continue;
                }
            };
            let Some(candidate) = parse_read_response(
                &search_candidate.category,
                &search_candidate.entry_id,
                &read_body,
            ) else {
                diagnostic.push_str("  read parse: no xmcd entry\n");
                continue;
            };
            append_candidate_diagnostic(&mut diagnostic, &candidate);
            if !candidate.disc_id.eq_ignore_ascii_case(&request.disc_id) {
                diagnostic.push_str("  rejected: DISCID mismatch\n");
                continue;
            }
            if request.year.is_some() && request.year != candidate.year {
                diagnostic.push_str("  rejected: year mismatch\n");
                continue;
            }
            if candidate.track_titles.len() != request.track_count {
                diagnostic.push_str("  rejected: track count mismatch\n");
                continue;
            }
            if !gnudb_track_titles_match(&request.track_titles_by_number, &candidate.track_titles) {
                diagnostic.push_str("  rejected: track titles did not align\n");
                continue;
            }
            accepted.push(candidate);
        }

        diagnostic.push_str(&format!("GnuDB accepted candidates: {}\n", accepted.len()));
        if accepted.is_empty() {
            Ok(DiscReleaseLookupResult::NotFound { diagnostic })
        } else {
            Ok(DiscReleaseLookupResult::Found {
                candidates: accepted,
                diagnostic,
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GnudbSearchCandidate {
    category: String,
    entry_id: String,
    title: String,
}

fn parse_search_response(body: &str) -> Vec<GnudbSearchCandidate> {
    let mut lines = body.lines().map(str::trim).filter(|line| !line.is_empty());
    let Some(first) = lines.next() else {
        return Vec::new();
    };
    let code = first.get(..3).unwrap_or_default();
    match code {
        "200" => parse_search_candidate_line(first.get(4..).unwrap_or_default())
            .into_iter()
            .collect(),
        "210" | "211" => lines
            .take_while(|line| *line != ".")
            .filter_map(parse_search_candidate_line)
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_search_candidate_line(line: &str) -> Option<GnudbSearchCandidate> {
    let mut parts = line.splitn(3, char::is_whitespace);
    let category = parts.next()?.trim();
    let entry_id = parts.next()?.trim();
    let title = parts.next().unwrap_or_default().trim();
    if category.is_empty() || entry_id.is_empty() {
        return None;
    }
    Some(GnudbSearchCandidate {
        category: category.to_owned(),
        entry_id: entry_id.to_owned(),
        title: title.to_owned(),
    })
}

fn parse_read_response(category: &str, entry_id: &str, body: &str) -> Option<DiscReleaseCandidate> {
    let mut fields = Vec::new();
    let mut art_ids = Vec::new();
    for line in body.lines().map(str::trim_end) {
        if line == "." {
            break;
        }
        if let Some(art_id) = line.strip_prefix("# Artid:") {
            let art_id = art_id.trim();
            if !art_id.is_empty() {
                art_ids.push(art_id.to_owned());
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        fields.push((key.trim().to_owned(), value.trim().to_owned()));
    }

    let disc_id = field_value(&fields, "DISCID")?;
    let (artist, title) = field_value(&fields, "DTITLE")
        .map(split_dtitle)
        .unwrap_or((None, None));
    let year = field_value(&fields, "DYEAR").and_then(first_year);
    let mut tracks = fields
        .iter()
        .filter_map(|(key, value)| {
            let index = key.strip_prefix("TTITLE")?.parse::<usize>().ok()?;
            Some((index, value.clone()))
        })
        .collect::<Vec<_>>();
    tracks.sort_by_key(|(index, _)| *index);

    Some(DiscReleaseCandidate {
        category: category.to_owned(),
        entry_id: entry_id.to_owned(),
        disc_id: disc_id.to_owned(),
        artist,
        title,
        year,
        track_titles: tracks.into_iter().map(|(_, title)| title).collect(),
        art_ids,
    })
}

fn field_value<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(field_key, _)| field_key.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.is_empty())
}

fn split_dtitle(value: &str) -> (Option<String>, Option<String>) {
    let Some((artist, title)) = value.split_once(" / ") else {
        return (
            None,
            Some(value.trim().to_owned()).filter(|value| !value.is_empty()),
        );
    };
    (
        Some(artist.trim().to_owned()).filter(|value| !value.is_empty()),
        Some(title.trim().to_owned()).filter(|value| !value.is_empty()),
    )
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

fn gnudb_track_titles_match(expected: &[(i64, String)], actual: &[String]) -> bool {
    if expected.is_empty() {
        return true;
    }
    expected.iter().all(|(number, title)| {
        let Some(actual_title) = usize::try_from(*number)
            .ok()
            .and_then(|number| number.checked_sub(1))
            .and_then(|index| actual.get(index))
        else {
            return false;
        };
        titles_match(title, actual_title)
    })
}

fn titles_match(left: &str, right: &str) -> bool {
    let left = normalize_text(left);
    let right = normalize_text(right);
    !left.is_empty() && (left == right || left.contains(&right) || right.contains(&left))
}

fn normalize_text(value: &str) -> String {
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

fn append_candidate_diagnostic(diagnostic: &mut String, candidate: &DiscReleaseCandidate) {
    diagnostic.push_str(&format!(
        "  read: disc_id={} artist={} title={} year={} tracks={} art_ids=[{}]\n",
        candidate.disc_id,
        candidate.artist.as_deref().unwrap_or("-"),
        candidate.title.as_deref().unwrap_or("-"),
        candidate
            .year
            .map(|year| year.to_string())
            .unwrap_or_else(|| "-".into()),
        candidate.track_titles.len(),
        candidate.art_ids.join(", ")
    ));
    for (index, title) in candidate.track_titles.iter().enumerate() {
        diagnostic.push_str(&format!("    TTITLE{}={}\n", index, title));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{parse_read_response, parse_search_response, GnudbDiscReleaseLookup};
    use crate::application::ports::{DiscReleaseLookup, DiscReleaseLookupResult};
    use crate::bootstrap::settings::GnudbSettings;

    #[test]
    fn parses_search_candidates() {
        let candidates = parse_search_response(
            "210 Found exact matches, list follows\nrock 9f0c2a0d Artist / Album\nmisc c60c9d10 Other / Album\n.\n",
        );

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].category, "rock");
        assert_eq!(candidates[0].entry_id, "9f0c2a0d");
        assert_eq!(candidates[0].title, "Artist / Album");
    }

    #[test]
    fn parses_xmcd_read_response_with_artids() {
        let candidate = parse_read_response(
            "rock",
            "9f0c2a0d",
            "210 rock 9f0c2a0d CD database entry follows\n# Artid: abc\n# Artid: def\nDISCID=9F0C2A0D\nDTITLE=Boney M. / Oceans Of Fantasy\nDYEAR=1979\nTTITLE0=Let It All Be Music\nTTITLE1=Gotta Go Home\n.\n",
        )
        .unwrap();

        assert_eq!(candidate.disc_id, "9F0C2A0D");
        assert_eq!(candidate.artist.as_deref(), Some("Boney M."));
        assert_eq!(candidate.title.as_deref(), Some("Oceans Of Fantasy"));
        assert_eq!(candidate.year, Some(1979));
        assert_eq!(candidate.track_titles.len(), 2);
        assert_eq!(candidate.art_ids, vec!["abc", "def"]);
    }

    #[tokio::test]
    async fn lookup_reads_and_filters_matching_candidate() {
        let (url, requests) = serve_sequence(vec![
            (
                "200 OK",
                "210 Found exact matches, list follows\nrock 9f0c2a0d Boney M. / Oceans Of Fantasy\n.\n",
            ),
            (
                "200 OK",
                "210 rock 9f0c2a0d CD database entry follows\n# Artid: 11111111-1111-1111-1111-111111111111\nDISCID=9F0C2A0D\nDTITLE=Boney M. / Oceans Of Fantasy\nDYEAR=1979\nTTITLE0=Let It All Be Music\nTTITLE1=Gotta Go Home\n.\n",
            ),
        ])
        .await;
        let lookup = lookup(url, true);

        let result = lookup
            .lookup_disc_release(crate::application::ports::DiscReleaseLookupRequest {
                disc_id: "9F0C2A0D".into(),
                artist: Some("Boney M.".into()),
                album_title: Some("Oceans Of Fantasy".into()),
                year: Some(1979),
                track_count: 2,
                track_titles_by_number: vec![
                    (1, "Let It All Be Music".into()),
                    (2, "Gotta Go Home".into()),
                ],
            })
            .await
            .unwrap();

        let DiscReleaseLookupResult::Found {
            candidates,
            diagnostic,
        } = result
        else {
            panic!("expected GnuDB match");
        };
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].art_ids,
            vec!["11111111-1111-1111-1111-111111111111"]
        );
        assert!(diagnostic.contains("GnuDB accepted candidates: 1"));
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn lookup_ignores_mismatched_discid() {
        let (url, _) = serve_sequence(vec![
            (
                "200 OK",
                "210 Found exact matches, list follows\nrock 9f0c2a0d Boney M. / Oceans Of Fantasy\n.\n",
            ),
            (
                "200 OK",
                "210 rock 9f0c2a0d CD database entry follows\nDISCID=00000000\nDTITLE=Boney M. / Oceans Of Fantasy\nDYEAR=1979\nTTITLE0=Let It All Be Music\n.\n",
            ),
        ])
        .await;
        let lookup = lookup(url, true);

        let result = lookup
            .lookup_disc_release(crate::application::ports::DiscReleaseLookupRequest {
                disc_id: "9F0C2A0D".into(),
                artist: Some("Boney M.".into()),
                album_title: Some("Oceans Of Fantasy".into()),
                year: Some(1979),
                track_count: 1,
                track_titles_by_number: vec![(1, "Let It All Be Music".into())],
            })
            .await
            .unwrap();

        let DiscReleaseLookupResult::NotFound { diagnostic } = result else {
            panic!("expected no GnuDB match");
        };
        assert!(diagnostic.contains("rejected: DISCID mismatch"));
    }

    #[tokio::test]
    async fn lookup_reports_api_errors_as_non_matching_result() {
        let url = serve_once("500 Internal Server Error", "boom").await;
        let lookup = lookup(url, true);

        let result = lookup
            .lookup_disc_release(crate::application::ports::DiscReleaseLookupRequest {
                disc_id: "9F0C2A0D".into(),
                artist: Some("Boney M.".into()),
                album_title: Some("Oceans Of Fantasy".into()),
                year: Some(1979),
                track_count: 1,
                track_titles_by_number: vec![(1, "Let It All Be Music".into())],
            })
            .await
            .unwrap();

        let DiscReleaseLookupResult::NotFound { diagnostic } = result else {
            panic!("expected no GnuDB match");
        };
        assert!(diagnostic.contains("HTTP 500"));
    }

    #[test]
    fn endpoint_url_is_built_from_server() {
        let lookup = lookup("4ckgj7jx.gnudb.org".into(), true);

        assert_eq!(
            lookup.endpoint_url(),
            "http://4ckgj7jx.gnudb.org/~cddb/cddb.cgi"
        );
    }

    fn lookup(server: String, enabled: bool) -> GnudbDiscReleaseLookup {
        GnudbDiscReleaseLookup::new(&GnudbSettings {
            disc_lookup_enabled: enabled,
            server,
            user_email: "user@example.com".into(),
        })
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
