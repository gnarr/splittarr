## [0.1.1](https://github.com/gnarr/splittarr/compare/v0.1.0...v0.1.1) (2022-09-24)


### Bug Fixes

* **settings:** allow loading of config from multiple locations ([5bc295d](https://github.com/gnarr/splittarr/commit/5bc295dbe6f84c53551b7c496d1106f53a21a0d9))



# [0.1.0](https://github.com/gnarr/splittarr/compare/d67e73e05c7eefb67c16a573361666e945ee6679...v0.1.0) (2022-09-24)


### Bug Fixes

* **cleanup:** ignore deletion errors ([38f92fc](https://github.com/gnarr/splittarr/commit/38f92fc4d3bf3dba14877a2c0b4d9ff063b9df70))
* **config:** make config.toml optional ([0724cc4](https://github.com/gnarr/splittarr/commit/0724cc4662903b624be08e4ce4291056fbd0343c))


### Features

* **config:** add shnsplit, env parsing and default for shnsplit ([0e9e420](https://github.com/gnarr/splittarr/commit/0e9e4200a468918069073142bfc362c7408c3674))
* **datastore:** make data_dir path configurable and move all table creation into establish_connection ([26e2b06](https://github.com/gnarr/splittarr/commit/26e2b06bf4a08828ea3bedfaaca8b2f13cf50568))
* **datastore:** store locations of created files for cleanup ([deb7511](https://github.com/gnarr/splittarr/commit/deb75118de531157ca8c6f0df8a0e44926c46a03))
* **docker:** add dockerfile ([f729656](https://github.com/gnarr/splittarr/commit/f7296567a70b8ab31283f3f8d131d8b5627af775))
* **logging:** add logging ([0320478](https://github.com/gnarr/splittarr/commit/03204780d5e25c3d7fcbbfa907347878e2456b42))
* cleanup of imported files and loop forever ([afa2a1a](https://github.com/gnarr/splittarr/commit/afa2a1a3251a7bcb45651ff098bcbda41c8566c1))
* **config:** add a basic config parser and an example config file ([d67e73e](https://github.com/gnarr/splittarr/commit/d67e73e05c7eefb67c16a573361666e945ee6679))
* **constants:** add dirs using ProjectDirs ([f62e956](https://github.com/gnarr/splittarr/commit/f62e956993709c68f1700d9de82daedc70e5f40f))
* **lidarr:** add support for featching queue ([b433cac](https://github.com/gnarr/splittarr/commit/b433cac8702420c5fd802e7886180a2b43478abc))
* **shnsplit:** parsing of cue files and passing to shnsplit. ([0be0a7e](https://github.com/gnarr/splittarr/commit/0be0a7ed5815011fd74cab8ead0865e71b6af065))
* **splittarr:** featch lidarr download queue, save to database and send to shnsplit ([b5fa554](https://github.com/gnarr/splittarr/commit/b5fa55473b18f547534cd09e05e8072163972a3d))
* **store:** add basic sqlite support ([583c6c8](https://github.com/gnarr/splittarr/commit/583c6c8e3c62c9396beab967a2114634b7db8e5f))



