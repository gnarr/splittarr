# Changelog

- - -
## [v0.4.0](https://github.com/gnarr/splittarr/compare/65c6f1eab5f055eacc9806d8eadfdb86a9210ddd..v0.4.0) - 2026-06-17
#### Features
- (**lidarr**) add disc-aware manual import fallback - ([7b1d3ab](https://github.com/gnarr/splittarr/commit/7b1d3ab2d809a8d6169eb3fdc1f90ce80c26854d)) - Gunnar Cortes
- (**lidarr**) trigger manual import after splitting - ([65c6f1e](https://github.com/gnarr/splittarr/commit/65c6f1eab5f055eacc9806d8eadfdb86a9210ddd)) - Gunnar Cortes
#### Bug Fixes
- (**lidarr**) parse cue hints from lossy paths - ([9b3d6f9](https://github.com/gnarr/splittarr/commit/9b3d6f96923961ccd58a1a410019dea23d37393b)) - Gunnar Cortes
- (**lidarr**) include existing split tracks in manual import - ([733dafe](https://github.com/gnarr/splittarr/commit/733dafe2192198405ff99013160757297a08da38)) - Gunnar Cortes
- handle cleanup and lookup edge cases - ([90ffbf9](https://github.com/gnarr/splittarr/commit/90ffbf964c31146fe89413b8c002c3609ae475b7)) - Gunnar Cortes

- - -

## [v0.3.5](https://github.com/gnarr/splittarr/compare/4f8848b2ff62dce95a6df32bb46cbe4df144c43f..v0.3.5) - 2026-06-13
#### Bug Fixes
- (**shnsplit**) reconcile generated track outputs (#27) - ([4f8848b](https://github.com/gnarr/splittarr/commit/4f8848b2ff62dce95a6df32bb46cbe4df144c43f)) - Gunnar Cortes

- - -

## [v0.3.4](https://github.com/gnarr/splittarr/compare/39c4f0b03f413e255a3307bf2b4a08e690ba5423..v0.3.4) - 2026-06-13
#### Bug Fixes
- (**shnsplit**) reject partial split output detection - ([39c4f0b](https://github.com/gnarr/splittarr/commit/39c4f0b03f413e255a3307bf2b4a08e690ba5423)) - Gunnar Cortes

- - -

## [v0.3.3](https://github.com/gnarr/splittarr/compare/3ecad71e8b7e9608743b4c496cdc1cb21648b6fc..v0.3.3) - 2026-06-13
#### Bug Fixes
- (**shnsplit**) preflight sanitized track renames - ([e2bb879](https://github.com/gnarr/splittarr/commit/e2bb8793e7e5b7fdf9f2bbf2be6f8e3eadb93126)) - Gunnar Cortes
- (**shnsplit**) sanitize generated track filenames - ([3ecad71](https://github.com/gnarr/splittarr/commit/3ecad71e8b7e9608743b4c496cdc1cb21648b6fc)) - Gunnar Cortes

- - -

## [v0.3.2](https://github.com/gnarr/splittarr/compare/db5d962e3ff9822d6ef6d520f9d86f9fb6f840a0..v0.3.2) - 2026-06-12
#### Bug Fixes
- (**shnsplit**) preserve non-utf8 track paths - ([62affca](https://github.com/gnarr/splittarr/commit/62affcad48c2b2b2f9afe48e98990ca4d5a8b634)) - Gunnar Cortes
#### Refactoring
- (**sqlite**) move db enum mappings out of domain - ([e02cb61](https://github.com/gnarr/splittarr/commit/e02cb614f13702e02f2126cb9a299d83affa4413)) - Gunnar Cortes
- (**web**) use async read store port - ([fa6ddfc](https://github.com/gnarr/splittarr/commit/fa6ddfc779f5429756f5e4fc9077c159cbcbf91e)) - Gunnar Cortes
- (**web**) decouple web ui read store - ([db5d962](https://github.com/gnarr/splittarr/commit/db5d962e3ff9822d6ef6d520f9d86f9fb6f840a0)) - Gunnar Cortes

- - -

## [v0.3.1](https://github.com/gnarr/splittarr/compare/7b41c14172402ad75bfe40cb5e2587dd8e087145..v0.3.1) - 2026-06-12
#### Bug Fixes
- (**processing**) log split error chains - ([f751a1b](https://github.com/gnarr/splittarr/commit/f751a1b294de505a24a35c0b980467c1e84af8b6)) - Gunnar Cortes
- (**processing**) log split failures - ([add86e3](https://github.com/gnarr/splittarr/commit/add86e369573d28a1f3be8e595a3801f690c4205)) - Gunnar Cortes
#### Continuous Integration
- bump cache and upload-artifact versions - ([0894de2](https://github.com/gnarr/splittarr/commit/0894de20e2cbe445fff04331e77076226f20875f)) - Gunnar Cortes
#### Refactoring
- (**adapter**) simplify blocking wrapper results - ([a114ba0](https://github.com/gnarr/splittarr/commit/a114ba029e049ae3bbe72f369000aab8e609017b)) - Gunnar Cortes
- (**adapter**) share cue filter matching helper - ([2264e10](https://github.com/gnarr/splittarr/commit/2264e100e79aa41ef991bafa674db0d06379671c)) - Gunnar Cortes
- (**adapter**) avoid duplicate audio input metadata lookups - ([8c38cbd](https://github.com/gnarr/splittarr/commit/8c38cbd90fbabc0d107c4315ddb1deb0f961c41d)) - Gunnar Cortes
- (**application**) batch cue file filtering behind inspector - ([cf8f671](https://github.com/gnarr/splittarr/commit/cf8f6714ff3dd4b486f7d5b7a0d94786714b4b0c)) - Gunnar Cortes
- (**application**) move track file size lookup behind cue inspector - ([3d8f57c](https://github.com/gnarr/splittarr/commit/3d8f57cc7827d241e9b066312d675e6925b9c8d5)) - Gunnar Cortes
- (**application**) extract cue input inspector - ([7b41c14](https://github.com/gnarr/splittarr/commit/7b41c14172402ad75bfe40cb5e2587dd8e087145)) - Gunnar Cortes

- - -

## [v0.3.0](https://github.com/gnarr/splittarr/compare/9123e413e4a036f917435821dcb8f69a3b0dfc09..v0.3.0) - 2026-06-02
#### Features
- (**downloads**) add persistent history ui - ([b698edc](https://github.com/gnarr/splittarr/commit/b698edc65683b6319fb75a552636e56b13675f2c)) - Gunnar Cortes
#### Bug Fixes
- (**cleanup**) persist top-level cleanup failures - ([fa6cd33](https://github.com/gnarr/splittarr/commit/fa6cd33171359f7aa68969aa68bb231c3c68c7bd)) - Gunnar Cortes
- (**cleanup**) fail downloads on silent delete errors - ([4f16668](https://github.com/gnarr/splittarr/commit/4f16668e47b8e6493f5607be6c5a8f448149df9d)) - Gunnar Cortes
- (**downloads**) harden tracked download persistence - ([d01adff](https://github.com/gnarr/splittarr/commit/d01adff974b00cb4f87036393d0d06841d9d5433)) - Gunnar Cortes
- (**lidarr**) paginate queue results for failed imports - ([fdf16e3](https://github.com/gnarr/splittarr/commit/fdf16e38a66569ecab9322edb567f111334a3ee5)) - Gunnar Cortes
- (**monitor**) reduce runtime and sqlite contention - ([6018263](https://github.com/gnarr/splittarr/commit/60182631b335ffa56f967dd88e38bdf180d38ad4)) - Gunnar Cortes
- (**runtime**) isolate monitor loop on its own thread - ([fdf9384](https://github.com/gnarr/splittarr/commit/fdf9384f122c74997b3c456cf307425cc8e657e2)) - Gunnar Cortes
- (**splittarr**) handle failed imports more robustly - ([0c49337](https://github.com/gnarr/splittarr/commit/0c49337bdb2f7c2e6186c45f8be06d6321de4961)) - Gunnar Cortes
- (**splitter**) make track snapshots best effort - ([f01c41a](https://github.com/gnarr/splittarr/commit/f01c41a9853a52f7f59f90507a3d25dc13cdde91)) - Gunnar Cortes
- (**store**) batch cleanup writes and harden state loading - ([0523505](https://github.com/gnarr/splittarr/commit/0523505efaa035e1049bf40d638dbc1eb8e4bb8f)) - Gunnar Cortes
- test data mismatch - ([b6f0955](https://github.com/gnarr/splittarr/commit/b6f0955e0ead87ea462d80370521b5f38eb0b284)) - Gunnar Cortes
- treating cue parsing for input-file snapshotting as best-effort - ([3e84fc2](https://github.com/gnarr/splittarr/commit/3e84fc2cc2e3a5e7f2bc82868ac9d17302365a86)) - Gunnar Cortes
- remove redundant touch_download_queue_presence() - ([db2196a](https://github.com/gnarr/splittarr/commit/db2196aa95b508a489c9d9f91a8d15d577e41240)) - Gunnar Cortes
- bind UI only to localhost - ([65728c7](https://github.com/gnarr/splittarr/commit/65728c7f1df4a06785b2ee39c60886e28ae6ca43)) - Gunnar Cortes
#### Performance
- (**downloads**) avoid extra cleanup graph lookups - ([8d00d0f](https://github.com/gnarr/splittarr/commit/8d00d0fd271fd1f9a7da72a4ae1a76a17b951877)) - Gunnar Cortes
- replace per-row polling loop with a single bulk history refresh endpoint. - ([b64d2e0](https://github.com/gnarr/splittarr/commit/b64d2e00129ef1820575a52730fa989dec0c6e5e)) - Gunnar Cortes
#### Refactoring
- (**architecture**) migrate to hexagonal modules - ([9123e41](https://github.com/gnarr/splittarr/commit/9123e413e4a036f917435821dcb8f69a3b0dfc09)) - Gunnar Cortes
- optimize graph loading from DB - ([7d12a08](https://github.com/gnarr/splittarr/commit/7d12a080bd84e16c15ca30bc846a81fa6fe4699f)) - Gunnar Cortes
#### Miscellaneous Chores
- add auth warning to README - ([673eac1](https://github.com/gnarr/splittarr/commit/673eac183ed74e05740e513cc75328412d49e7b2)) - Gunnar Cortes
- remove unused import - ([b597cce](https://github.com/gnarr/splittarr/commit/b597cce8598febb76565fd27dd15c277b2e4cec4)) - Gunnar Cortes
- remove unused code - ([0100915](https://github.com/gnarr/splittarr/commit/010091592b2e3dde3a88faf428c2f901d6bcf71c)) - Gunnar Cortes
- remove tokio signal from Cargo - ([000ac7d](https://github.com/gnarr/splittarr/commit/000ac7dba61d95a34b6584a73eecc3f419a72a05)) - Gunnar Cortes
- use 127.0.0.1 as default bind_address in config.toml.example - ([26439dd](https://github.com/gnarr/splittarr/commit/26439dd2ee9c912454d6214ef313684003a83d3f)) - Gunnar Cortes
- use 127.0.0.1 for all examples in README - ([70c7e13](https://github.com/gnarr/splittarr/commit/70c7e1398d108c791d1150866600e58d90c92577)) - Gunnar Cortes
- update default UI listen in README - ([cea6ba3](https://github.com/gnarr/splittarr/commit/cea6ba3f246ea6c04f3574e5abcfebdc7805ab98)) - Gunnar Cortes
#### Style
- (**web**) match rooterr ui theme - ([2735e0b](https://github.com/gnarr/splittarr/commit/2735e0beebcab6b44b64ad98af5026ace03e9e1f)) - Gunnar Cortes

- - -

## [v0.2.6](https://github.com/gnarr/splittarr/compare/4b6c1a77545c53a7508a6f6107d6dfa492a20fc8..v0.2.6) - 2026-05-27
#### Bug Fixes
- (**ci**) print cocogitto logs - ([7d60afc](https://github.com/gnarr/splittarr/commit/7d60afc0400cb7b632af3c37488b4e155a34f0ca)) - Gunnar Cortes
- (**lidarr**) indexer can be undefined - ([4b6c1a7](https://github.com/gnarr/splittarr/commit/4b6c1a77545c53a7508a6f6107d6dfa492a20fc8)) - Gunnar Cortes Heimisson
- force rebuild - ([7873c54](https://github.com/gnarr/splittarr/commit/7873c5413487d9131c73972cef6e4d0f66a85d57)) - Gunnar Cortes
- update README and add license files - ([cd2db36](https://github.com/gnarr/splittarr/commit/cd2db36fec99c7433ea527e63343ed9283435a8b)) - Gunnar Cortes
#### Documentation
- (**changelog**) add cocogitto insertion separator - ([c9c6009](https://github.com/gnarr/splittarr/commit/c9c600978a1149830e666690b21ba60a6be76367)) - Gunnar Cortes
#### Continuous Integration
- (**release**) configure git author before release bump - ([90eb12e](https://github.com/gnarr/splittarr/commit/90eb12e80bd99718196d45527d5f3caadc0290ce)) - Gunnar Cortes
- (**release**) use cocogitto action for commit check - ([3f9708a](https://github.com/gnarr/splittarr/commit/3f9708a7d89bb72e8ea10453dd909e1aa3e497d9)) - Gunnar Cortes
- (**release**) modernize docker release pipeline (#9) - ([fb317c3](https://github.com/gnarr/splittarr/commit/fb317c3cfb7549402d2eea3e88986726cc0b7416)) - Gunnar Cortes
#### Refactoring
- (**core**) rewrite processing pipeline (#8) - ([8523fe7](https://github.com/gnarr/splittarr/commit/8523fe766a327a0004fe6c323131196473c5f68d)) - Gunnar Cortes

- - -


## [0.2.5](https://github.com/gnarr/splittarr/compare/v0.2.4...v0.2.5) (2024-01-03)


### Bug Fixes

* **lidarr:** support v4 api ([9843fdd](https://github.com/gnarr/splittarr/commit/9843fdd8392e1869ffe5849c4f5f3a605779a874))
* check audio file existence before listing as source for shnsplit ([426455a](https://github.com/gnarr/splittarr/commit/426455a694a61628fbdb938713535a0014141258))



## [0.2.4](https://github.com/gnarr/splittarr/compare/v0.2.3...v0.2.4) (2022-12-01)


### Bug Fixes

* **shnsplit:** add default values for cue unwrap ([f1322dd](https://github.com/gnarr/splittarr/commit/f1322dd387b6b024c8f6e114106bf5561b7e7ee0))



## [0.2.3](https://github.com/gnarr/splittarr/compare/v0.2.2...v0.2.3) (2022-10-10)


### Bug Fixes

* **lidarr:** album_id is optional ([db44c9c](https://github.com/gnarr/splittarr/commit/db44c9c458390f3d1c9d4412c2ae94d919bd0b34))



## [0.2.2](https://github.com/gnarr/splittarr/compare/v0.2.1...v0.2.2) (2022-09-25)


### Bug Fixes

* **shnsplit:** set cue parsing to not strict by default ([584467c](https://github.com/gnarr/splittarr/commit/584467ccc9c070b21c78384ec65d07374692c1c2))



## [0.2.1](https://github.com/gnarr/splittarr/compare/v0.2.0...v0.2.1) (2022-09-25)


### Bug Fixes

* **lidarr:** indexer is optional ([60a470a](https://github.com/gnarr/splittarr/commit/60a470aab95eb4b3d632834c4445895b1380edaf))



