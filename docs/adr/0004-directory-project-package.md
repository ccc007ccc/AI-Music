# Use a directory project package

AI Music stores durable work in a directory ending in `.aimusic`. The bundle
contains a small identity/resource `manifest.json`, the sole editable musical
source `project.json`, and fixed `assets/`, `exports/`, `renders/`, and
`history/` directories.

This keeps source data, licensed instrument references, interchange files,
renders, and AI revision evidence together without forcing large binary assets
into the source JSON or repository. A directory rather than a zip allows
incremental saves, native file access, and future asset caching. Each source
save is written through a temporary file and renamed atomically. Legacy loose
`.json` projects remain accepted by the CLI for compatibility, but new desktop
projects are always packages.
