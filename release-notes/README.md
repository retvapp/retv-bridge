# Release notes

One file per release: `release-notes/<version>.json`, where `<version>` is exactly the version of the tag
`bridge-v<version>` (and of `app/tauri.conf.json`, `Cargo.toml` and `Cargo.lock`: `scripts/release-version.sh --set
<version>` writes all three).

```json
{
  "en": ["Finds TVs on the network faster", "Fixes the tray icon on dark menu bars"],
  "ru": ["Быстрее находит ТВ в сети", "Исправлен значок в тёмной строке меню"],
  "uk": ["Швидше знаходить ТБ у мережі", "Виправлено значок у темному рядку меню"]
}
```

- `en` and `ru` are required, `uk` is optional.
- Each is a list of 1 to 8 lines. A line is plain text (no Markdown), not blank, at most 200 characters.
- `en` has no Cyrillic: retv.app shows only `en`, and the world site carries no Cyrillic at all. ru.retv.app shows `ru`,
  `en` and `uk`.
- The version follows semver; a pre-release uses dots only (`1.1.0-rc.1`, not `1.1.0-rc-1`).

The `check` job of `.github/workflows/release.yml` runs `scripts/release-version.sh` on the tag before any build, so a
missing or malformed file fails the release within a minute. `scripts/release-retv-bridge.sh` reads the same file when
it writes the download manifests.
