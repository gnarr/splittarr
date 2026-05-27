# splittarr
Splits single-file albums for Lidarr using included CUE files, then deletes generated split files after Lidarr imports them.

Splittarr watches Lidarr's queue for completed downloads whose import failed. If the download directory contains CUE files, Splittarr runs `shnsplit` against the referenced audio file, records the generated tracks in SQLite, and waits for Lidarr to import them. When the queue item disappears, Splittarr removes only the generated tracks it recorded.

## Configuration
Splittarr reads configuration from these sources, in order:

1. built-in defaults
2. `config.toml` in the current directory
3. `/config/config.toml`
4. the platform config directory, such as `~/.config/splittarr/config.toml`
5. the file passed with `-c/--config`
6. environment variables prefixed with `SPLITTARR_`

Nested environment keys use a double underscore, for example:

```bash
SPLITTARR_CHECK_FREQUENCY_SECONDS=30
SPLITTARR_LIDARR__URL=http://lidarr:8686
SPLITTARR_LIDARR__API_KEY=...
SPLITTARR_SHNSPLIT__PATH=/usr/bin/shnsplit
```

## docker-compose example

```yaml
services:
  splittarr:
    container_name: splittarr
    image: gnarr/splittarr:latest
    environment:
      SPLITTARR_LIDARR__URL: http://lidarr:8686
      SPLITTARR_LIDARR__API_KEY: ${LIDARR_API_KEY}
      SPLITTARR_SHNSPLIT__PATH: /usr/bin/shnsplit
    logging:
      driver: json-file
    restart: unless-stopped
    volumes:
      - ${CONFIG_ROOT:-/etc/rangerr}/splittarr:/config
      - media:/data

volumes:
  media:
    driver: local
    driver_opts:
      type: none
      o: bind
      device: "${MEDIA_ROOT:-/media/library}"

```
