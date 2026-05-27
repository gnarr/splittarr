# splittarr
Splits single file albums for Lidarr using included CUE files - Deletes split files after import

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