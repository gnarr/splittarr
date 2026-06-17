# Splittarr

Splittarr is a small companion service for [Lidarr](https://lidarr.audio/) that handles albums delivered as a single audio file plus one or more CUE sheets.

Lidarr usually expects individual track files. When a single-file album fails to import, Splittarr detects that failed queue item, finds the CUE file, splits the referenced audio into FLAC tracks with `shnsplit`, records the full processing history in SQLite, and removes only those generated files after Lidarr has imported them.

## What it does

Splittarr continuously polls Lidarr's download queue and looks for queue records where:

- the download status is `completed`
- the tracked download state is `importFailed`
- the record has a download ID
- the record has an output path

For each matching download, Splittarr:

1. scans the download output directory recursively for `.cue` files
2. parses each CUE file
3. checks that the CUE file references an audio file in the same directory
4. runs `shnsplit` in that directory
5. stores input snapshots, cue-sheet results, generated track paths, file sizes, and errors in a SQLite database
6. serves a small built-in web UI showing tracked downloads, statuses, and detail pages
7. waits for the download to disappear from Lidarr's queue
8. deletes the generated tracks it previously recorded
9. keeps the download row in the database and marks it `completed`

Splittarr does **not** delete the original album file, the CUE file, or arbitrary files in the download directory. Cleanup is based on the exact generated track paths recorded during splitting.

## Why this exists

A common problematic album layout looks like this:

```text
Album/
├── album.cue
└── album.flac
````

Lidarr may fail to import this because it wants separate track files. Splittarr turns it into something like:

```text
Album/
├── album.cue
├── album.flac
├── Artist - Album - 01 - First Track.flac
├── Artist - Album - 02 - Second Track.flac
└── Artist - Album - 03 - Third Track.flac
```

Lidarr can then import the generated tracks. Once Lidarr no longer reports the download in its queue, Splittarr deletes the generated split files so the download directory is cleaned up while keeping the full history in its database and UI.

Splittarr serves a built-in monitoring UI. By default it listens on `127.0.0.1:9899`.
The UI does not implement authentication, so avoid binding it to a public interface.
The UI includes:

- a download history page at `/`
- a detail page per tracked download at `/downloads/{download_id}`
- lifecycle state, Lidarr state, cue results, input and output file snapshots, file sizes, and cleanup status

## Requirements

When running Splittarr directly on the host, these tools need to be installed and available on `PATH`:

* `shnsplit`
* `flac`

On Debian/Ubuntu-based systems, these are typically provided by:

```bash
sudo apt install shntool flac cuetools
```

When running the Docker image, the required tools are already installed in the image.

## Docker Compose

```yaml
services:
  splittarr:
    image: gnarr/splittarr:latest
    container_name: splittarr
    restart: unless-stopped

    environment:
      SPLITTARR_LIDARR__URL: http://lidarr:8686
      SPLITTARR_LIDARR__API_KEY: ${LIDARR_API_KEY}

      # Optional, defaults shown:
      SPLITTARR_DATA_DIR: /config
      SPLITTARR_CHECK_FREQUENCY_SECONDS: 60
      SPLITTARR_SERVER__BIND_ADDRESS: 127.0.0.1:9899
      SPLITTARR_LOGGING__DOWNLOAD_LOG_ENABLED: "true"
      SPLITTARR_LIDARR__MANUAL_IMPORT_ENABLED: "false"
      SPLITTARR_GNUDB__DISC_LOOKUP_ENABLED: "false"
      SPLITTARR_GNUDB__SERVER: gnudb.gnudb.org
      SPLITTARR_GNUDB__USER_EMAIL: ""
      SPLITTARR_CUE__STRICT: "false"
      SPLITTARR_SHNSPLIT__PATH: shnsplit
      SPLITTARR_SHNSPLIT__OVERWRITE: "true"
      SPLITTARR_SHNSPLIT__FORMAT: "%p - %a - %n - %t"

    volumes:
      - ./config:/config
      - /path/to/media:/data
```

The important bit is that Splittarr must see the same download paths that Lidarr reports in its queue.

For example, if Lidarr reports a failed download path as:

```text
/data/downloads/Artist/Album
```

then Splittarr must also be able to access that exact path inside its container.

## Configuration

Splittarr can be configured with a TOML file, environment variables, or both.

Configuration is loaded in this order, with later sources overriding earlier ones:

1. built-in defaults
2. `config.toml` in the current directory
3. `/config/config.toml`
4. the platform config directory, for example `~/.config/splittarr/config.toml`
5. the file passed with `-c` or `--config`
6. environment variables prefixed with `SPLITTARR_`

### Example config file

```toml
data_dir = "/config"
check_frequency_seconds = 60

[server]
bind_address = "127.0.0.1:9899"

[logging]
download_log_enabled = true

[gnudb]
disc_lookup_enabled = false
# Use "gnudb.gnudb.org", your signup host like "7vrcg0sd.gnudb.org",
# or just the unique signup code like "7vrcg0sd".
server = "gnudb.gnudb.org"
user_email = ""

[musicbrainz]
disc_lookup_enabled = true
base_url = "https://musicbrainz.org"
trust_disc_lookup = false
add_missing_release_group_enabled = false

[lidarr]
url = "http://lidarr:8686"
api_key = "your-lidarr-api-key"
manual_import_enabled = true

[cue]
strict = false

[shnsplit]
path = "shnsplit"
overwrite = true
format = "%p - %a - %n - %t"
```

Run with an explicit config file:

```bash
splittarr --config /path/to/config.toml
```

or:

```bash
splittarr -c /path/to/config.toml
```

### Environment variables

Nested configuration keys use a double underscore.

```bash
export SPLITTARR_LIDARR__URL=http://lidarr:8686
export SPLITTARR_LIDARR__API_KEY=your-lidarr-api-key
export SPLITTARR_LIDARR__MANUAL_IMPORT_ENABLED=true
export SPLITTARR_LOGGING__DOWNLOAD_LOG_ENABLED=true
export SPLITTARR_GNUDB__DISC_LOOKUP_ENABLED=false
export SPLITTARR_GNUDB__SERVER=gnudb.gnudb.org
export SPLITTARR_GNUDB__USER_EMAIL=user@example.com
export SPLITTARR_MUSICBRAINZ__DISC_LOOKUP_ENABLED=true
export SPLITTARR_MUSICBRAINZ__BASE_URL=https://musicbrainz.org
export SPLITTARR_MUSICBRAINZ__TRUST_DISC_LOOKUP=false
export SPLITTARR_MUSICBRAINZ__ADD_MISSING_RELEASE_GROUP_ENABLED=false
export SPLITTARR_CHECK_FREQUENCY_SECONDS=60
export SPLITTARR_SERVER__BIND_ADDRESS=127.0.0.1:9899
export SPLITTARR_SHNSPLIT__FORMAT="%p - %a - %n - %t"

splittarr
```

## Configuration reference

| Setting                   | Environment variable                | Default                                | Description                                                |
| ------------------------- | ----------------------------------- | -------------------------------------- | ---------------------------------------------------------- |
| `data_dir`                | `SPLITTARR_DATA_DIR`                | platform data dir, `/config` in Docker | Directory used for Splittarr's SQLite database.            |
| `check_frequency_seconds` | `SPLITTARR_CHECK_FREQUENCY_SECONDS` | `60`                                   | How often Splittarr polls Lidarr's queue.                  |
| `server.bind_address`     | `SPLITTARR_SERVER__BIND_ADDRESS`    | `127.0.0.1:9899`                       | Address for the built-in web UI and health endpoint.       |
| `logging.download_log_enabled` | `SPLITTARR_LOGGING__DOWNLOAD_LOG_ENABLED` | `true` | Whether Splittarr writes `splittarr.log` into processed download folders. |
| `gnudb.disc_lookup_enabled` | `SPLITTARR_GNUDB__DISC_LOOKUP_ENABLED` | `false` | Whether Splittarr may use CUE `REM DISCID` values to ask GnuDB for release-selection hints. |
| `gnudb.server`            | `SPLITTARR_GNUDB__SERVER`           | `gnudb.gnudb.org`                       | GnuDB hostname or signup code, for example `7vrcg0sd.gnudb.org` or `7vrcg0sd`. |
| `gnudb.user_email`        | `SPLITTARR_GNUDB__USER_EMAIL`       | empty                                  | Email used in GnuDB's required `hello` field; required when GnuDB lookup is enabled. |
| `musicbrainz.disc_lookup_enabled` | `SPLITTARR_MUSICBRAINZ__DISC_LOOKUP_ENABLED` | `true` | Whether Splittarr may calculate a MusicBrainz Disc ID from CUE/audio lengths and query MusicBrainz before GnuDB fallback. |
| `musicbrainz.base_url` | `SPLITTARR_MUSICBRAINZ__BASE_URL` | `https://musicbrainz.org` | MusicBrainz base URL. |
| `musicbrainz.trust_disc_lookup` | `SPLITTARR_MUSICBRAINZ__TRUST_DISC_LOOKUP` | `false` | Whether a successful MusicBrainz Disc ID match may override the initial CUE-title album match and choose another compatible Lidarr album/release for the same artist. |
| `musicbrainz.add_missing_release_group_enabled` | `SPLITTARR_MUSICBRAINZ__ADD_MISSING_RELEASE_GROUP_ENABLED` | `false` | Whether Splittarr may add a missing Lidarr album for the same artist from a single MusicBrainz release-group Disc ID result before manual-import fallback gives up. |
| `lidarr.url`              | `SPLITTARR_LIDARR__URL`             | required                               | Base URL for Lidarr, for example `http://lidarr:8686`.     |
| `lidarr.api_key`          | `SPLITTARR_LIDARR__API_KEY`         | required                               | Lidarr API key.                                            |
| `lidarr.manual_import_enabled` | `SPLITTARR_LIDARR__MANUAL_IMPORT_ENABLED` | `true` | Whether Splittarr should ask Lidarr to manually import generated tracks after splitting. |
| `cue.strict`              | `SPLITTARR_CUE__STRICT`             | `false`                                | Whether CUE parsing should run in strict mode.             |
| `shnsplit.path`           | `SPLITTARR_SHNSPLIT__PATH`          | `shnsplit`                             | Path to the `shnsplit` executable.                         |
| `shnsplit.overwrite`      | `SPLITTARR_SHNSPLIT__OVERWRITE`     | `true`                                 | Whether `shnsplit` should overwrite existing output files. |
| `shnsplit.format`         | `SPLITTARR_SHNSPLIT__FORMAT`        | `%p - %a - %n - %t`                    | Output filename format passed to `shnsplit -t`.            |

MusicBrainz lookup is enabled by default. Splittarr reads referenced WAV/FLAC lengths, calculates a true MusicBrainz Disc ID, asks MusicBrainz `/ws/2/discid`, and selects a Lidarr release when MusicBrainz and Lidarr agree on a compatible release. If MusicBrainz is disabled or inconclusive, Splittarr falls back to GnuDB.

`musicbrainz.trust_disc_lookup` is a stronger, separate opt-in. Leave it `false` if Splittarr should only use MusicBrainz as a release-ID tie breaker inside the album Lidarr already matched from the CUE/download title. Set it to `true` if you want a MusicBrainz Disc ID match to be trusted enough to search the same Lidarr artist for another album whose title matches the MusicBrainz release or release-group title. Even when enabled, Splittarr still requires compatible track counts and still runs the final generated-track-to-Lidarr-track mapping before starting manual import.

`musicbrainz.add_missing_release_group_enabled` is disabled by default because it can change your Lidarr library. When enabled, if MusicBrainz Disc ID lookup returns releases that all belong to one release group and Splittarr cannot find a compatible Lidarr release, Splittarr asks Lidarr for `lidarr:<release-group-mbid>`, adds that album for the same artist as unmonitored, does not trigger a Lidarr search/download, and then tries the manual import again against the newly added album.

GnuDB lookup only uses 8-character CDDB/freeDB-style `REM DISCID` values from CUE files. If GnuDB registration says to change `gnudb.gnudb.org` to `<code>.gnudb.org`, put either that hostname or just `<code>` in `gnudb.server`; Splittarr builds the required plain HTTP CDDB endpoint internally.

## Output filename format

Splittarr passes `shnsplit.format` directly to `shnsplit -t`.

The default is:

```text
%p - %a - %n - %t
```

Common placeholders include:

| Placeholder | Meaning      |
| ----------- | ------------ |
| `%p`        | performer    |
| `%a`        | album        |
| `%n`        | track number |
| `%t`        | track title  |

So the default format creates filenames like:

```text
Artist - Album - 01 - Track Title.flac
```

## Overwrite behavior

By default, Splittarr runs `shnsplit` with overwrite enabled.

That means generated files with the same names may be overwritten. This is usually what you want for repeated processing of the same failed download, but it is worth being aware of.

To disable overwriting:

```toml
[shnsplit]
overwrite = false
```

or:

```bash
SPLITTARR_SHNSPLIT__OVERWRITE=false
```

## How cleanup works

Splittarr keeps a SQLite database in `data_dir/data.db`.

It stores:

* Lidarr download IDs
* lifecycle state and timestamps
* discovered CUE files
* input file snapshots and sizes
* split status per CUE file
* generated track paths and sizes
* cleanup status per generated track
* the last processing error, if any

When a tracked download disappears from Lidarr's queue, Splittarr assumes Lidarr has either imported it or no longer needs it. Splittarr then deletes only the generated tracks recorded in its database.

If a generated track is already gone, Splittarr records that as `missing` and continues cleanup. Tracked downloads are never deleted from the database.

## Running locally

Build:

```bash
cargo build --release
```

Run with a config file:

```bash
cargo run -- --config config.toml
```

Run tests:

```bash
cargo test
```

## Troubleshooting

### Splittarr finds no CUE files

Check that the path reported by Lidarr exists from Splittarr's point of view.

This is especially common with Docker. Lidarr and Splittarr need compatible volume mappings. If Lidarr reports `/data/downloads/foo`, Splittarr must also be able to read `/data/downloads/foo`.

### Lidarr API requests fail

Check:

* `SPLITTARR_LIDARR__URL`
* `SPLITTARR_LIDARR__API_KEY`
* container networking
* whether Lidarr is reachable from the Splittarr container

For Docker Compose, using the service name usually works:

```yaml
SPLITTARR_LIDARR__URL: http://lidarr:8686
```

### Splitting fails

Check that:

* `shnsplit` is installed
* `flac` is installed
* the CUE file references an audio file in the same directory
* the referenced audio filename matches exactly
* Splittarr has write permission in the download directory

### Files are split but Lidarr still does not import them

Splittarr only creates track files. Lidarr still needs to be able to see and import those files itself.

Check that Lidarr and Splittarr share the same media/download volume paths.

### Generated files are not cleaned up

Cleanup happens after the download disappears from Lidarr's queue.

If the item remains in Lidarr's queue, Splittarr keeps the generated files in place so Lidarr can still import them.

## Notes and limitations

* Splittarr is designed for single-file albums with CUE sheets.
* CUE files are searched recursively inside the failed download output path.
* A CUE file is skipped if it does not reference an existing audio file in its own directory.
* Generated files are FLAC files.
* Splittarr runs continuously; it is not a one-shot CLI tool.
* Splittarr only processes Lidarr queue items with `status = completed` and `trackedDownloadState = importFailed`.

## License

Licensed under either of:

- Apache License, Version 2.0
- MIT License

at your option.
