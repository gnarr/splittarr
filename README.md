# splittarr
Splits single file albums for Lidarr using included CUE files - Deletes split files after import

## docker-compose example
Using the hotio.dev based image in `docker/hotio/linux-amd64.Dockerfile`
```yaml
services:
  splittarr:
    container_name: splittarr
    image: splittarr-amd64:latest
    environment:
    - PUID=${SPLITTARR_PUID:-1000}
    - PGID=${SPLITTARR_PGID:-1000}
    - UMASK=${SPLITTARR_UMASK:-002}
    - TZ=${TIMEZONE:-Etc/UTC}
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