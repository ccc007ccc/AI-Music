# ADR 0007: Bind render source assets in the project manifest

## Status

Accepted.

## Context

`project.json` is the editable MIDI source, but a rendered piano also depends
on a licensed SF2/SFZ asset. Keeping that choice only in a CLI flag makes a
desktop project silently change timbre after reopening. Copying a complete
sample library into every project is wasteful and makes license boundaries
unclear.

## Decision

The `.aimusic` manifest may contain `source_assets`, keyed by a stable role
such as `instrument:piano`. Each binding records the asset ID, display name,
manifest location, license source, and attribution. Locations are either an
external absolute path or a path below the package's `assets/` directory.
`ProjectPackage` validates and resolves these references; package-relative
references cannot escape the bundle.

The desktop and CLI load the bound asset before rendering. A missing or invalid
bound resource is an explicit error. The built-in Rust piano remains the
default when a package has no binding. SFZ loading is still performed on the
control thread and is optimized to preload only layers reachable by the
current project performance. Duplicating a package preserves external bindings
and copies its package-local `assets/` tree as one package operation; a failed
copy removes only the newly-created destination.

## Consequences

- Reopening a project preserves its intended piano resource instead of silently
  changing the sound engine.
- Large libraries can remain user-managed external assets while the project
  retains license and attribution metadata.
- A project is not fully portable until its external bindings are relinked or
  the referenced asset is installed below `assets/`; future UI work can add a
  relink/import action without changing the render seam.
- Asset selection is a resource configuration change, not a MIDI edit, so it
  is recorded in the manifest rather than the Project revision history.
