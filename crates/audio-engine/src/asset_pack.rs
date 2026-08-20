//! Licensed external instrument assets loaded before rendering begins.
//!
//! [`AssetPack::load`] is the seam used by CLI, desktop, and sampler adapters.
//! It owns manifest parsing, path safety, license metadata checks,
//! and entry discovery so none of that work leaks into the audio thread.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

pub const ASSET_PACK_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetPackManifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub instrument_id: String,
    pub engine: AssetPackEngine,
    pub entry: String,
    pub license: AssetLicense,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssetPackEngine {
    #[serde(rename = "soundfont2")]
    SoundFont2,
    #[serde(rename = "sfz")]
    Sfz,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetLicense {
    #[serde(default)]
    pub spdx: Option<String>,
    pub name: String,
    pub source: String,
    pub attribution: String,
}

/// A validated manifest paired with its resolved entry file.
#[derive(Clone, Debug)]
pub struct AssetPack {
    manifest: AssetPackManifest,
    manifest_path: PathBuf,
    entry_path: PathBuf,
}

impl AssetPack {
    /// Reads and validates a pack on the caller's thread.
    ///
    /// Call this during application setup, never from an audio callback.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, AssetPackError> {
        let requested_path = path.as_ref();
        let manifest_path =
            fs::canonicalize(requested_path).map_err(|source| AssetPackError::ReadManifest {
                path: requested_path.to_owned(),
                source,
            })?;
        let json =
            fs::read_to_string(&manifest_path).map_err(|source| AssetPackError::ReadManifest {
                path: manifest_path.clone(),
                source,
            })?;
        let mut manifest: AssetPackManifest =
            serde_json::from_str(&json).map_err(|source| AssetPackError::ParseManifest {
                path: manifest_path.clone(),
                source,
            })?;
        validate_and_normalize(&mut manifest)?;

        let relative_entry = Path::new(&manifest.entry);
        validate_entry_path(relative_entry)?;
        validate_entry_extension(manifest.engine, relative_entry)?;
        let root = manifest_path.parent().unwrap_or_else(|| Path::new("."));
        let requested_entry_path = root.join(relative_entry);
        let entry_path = fs::canonicalize(&requested_entry_path).map_err(|source| {
            AssetPackError::ReadEntry {
                path: requested_entry_path.clone(),
                source,
            }
        })?;
        if !entry_path.starts_with(root) {
            return Err(AssetPackError::EntryEscapesPack { path: entry_path });
        }
        let metadata = fs::metadata(&entry_path).map_err(|source| AssetPackError::ReadEntry {
            path: entry_path.clone(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(AssetPackError::EntryNotFile(entry_path));
        }

        Ok(Self {
            manifest,
            manifest_path,
            entry_path,
        })
    }

    pub fn manifest(&self) -> &AssetPackManifest {
        &self.manifest
    }

    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    pub fn entry_path(&self) -> &Path {
        &self.entry_path
    }
}

#[derive(Debug, Error)]
pub enum AssetPackError {
    #[error("could not read asset pack manifest {path:?}: {source}")]
    ReadManifest {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not parse asset pack manifest {path:?}: {source}")]
    ParseManifest {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("unsupported asset pack schema version {0}")]
    UnsupportedSchema(u32),
    #[error("asset pack field '{field}' {reason}")]
    InvalidField {
        field: &'static str,
        reason: &'static str,
    },
    #[error("asset pack entry must be a relative path inside the pack: {0}")]
    InvalidEntryPath(String),
    #[error("asset pack entry {path:?} does not match engine {engine:?}")]
    EntryEngineMismatch {
        engine: AssetPackEngine,
        path: PathBuf,
    },
    #[error("could not read asset pack entry {path:?}: {source}")]
    ReadEntry {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("asset pack entry is not a file: {0:?}")]
    EntryNotFile(PathBuf),
    #[error("asset pack entry resolves outside its pack: {path:?}")]
    EntryEscapesPack { path: PathBuf },
}

fn validate_and_normalize(manifest: &mut AssetPackManifest) -> Result<(), AssetPackError> {
    if manifest.schema_version != ASSET_PACK_SCHEMA_VERSION {
        return Err(AssetPackError::UnsupportedSchema(manifest.schema_version));
    }
    normalize_id("id", &mut manifest.id)?;
    normalize_nonempty("name", &mut manifest.name)?;
    normalize_id("instrument_id", &mut manifest.instrument_id)?;
    normalize_nonempty("entry", &mut manifest.entry)?;
    normalize_nonempty("license.name", &mut manifest.license.name)?;
    normalize_nonempty("license.source", &mut manifest.license.source)?;
    normalize_nonempty("license.attribution", &mut manifest.license.attribution)?;
    if let Some(spdx) = &mut manifest.license.spdx {
        normalize_nonempty("license.spdx", spdx)?;
    }
    Ok(())
}

fn normalize_nonempty(field: &'static str, value: &mut String) -> Result<(), AssetPackError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AssetPackError::InvalidField {
            field,
            reason: "must not be empty",
        });
    }
    if trimmed.len() != value.len() {
        *value = trimmed.to_owned();
    }
    Ok(())
}

fn normalize_id(field: &'static str, value: &mut String) -> Result<(), AssetPackError> {
    normalize_nonempty(field, value)?;
    if value.starts_with('.')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(AssetPackError::InvalidField {
            field,
            reason: "may only contain ASCII letters, numbers, '.', '-' and '_'",
        });
    }
    Ok(())
}

fn validate_entry_path(entry: &Path) -> Result<(), AssetPackError> {
    if entry.as_os_str().is_empty()
        || entry.is_absolute()
        || entry.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(AssetPackError::InvalidEntryPath(
            entry.to_string_lossy().into_owned(),
        ));
    }
    Ok(())
}

fn validate_entry_extension(engine: AssetPackEngine, entry: &Path) -> Result<(), AssetPackError> {
    let extension = entry
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let matches_engine = match engine {
        AssetPackEngine::SoundFont2 => extension.eq_ignore_ascii_case("sf2"),
        AssetPackEngine::Sfz => extension.eq_ignore_ascii_case("sfz"),
    };
    if !matches_engine {
        return Err(AssetPackError::EntryEngineMismatch {
            engine,
            path: entry.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn create() -> Self {
            loop {
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let serial = NEXT_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "ai-music-asset-pack-{}-{timestamp}-{serial}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("could not create test directory {path:?}: {error}"),
                }
            }
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn manifest(entry: &str) -> AssetPackManifest {
        AssetPackManifest {
            schema_version: ASSET_PACK_SCHEMA_VERSION,
            id: "test-piano".to_owned(),
            name: "Test Piano".to_owned(),
            instrument_id: "piano".to_owned(),
            engine: AssetPackEngine::SoundFont2,
            entry: entry.to_owned(),
            license: AssetLicense {
                spdx: Some("CC-BY-3.0".to_owned()),
                name: "Creative Commons Attribution 3.0".to_owned(),
                source: "https://example.test/piano".to_owned(),
                attribution: "Test Piano authors".to_owned(),
            },
        }
    }

    fn write_manifest(directory: &TestDirectory, manifest: &AssetPackManifest) -> PathBuf {
        let path = directory.path().join("pack.json");
        fs::write(&path, serde_json::to_vec_pretty(manifest).unwrap()).unwrap();
        path
    }

    #[test]
    fn loads_and_resolves_a_valid_pack() {
        let directory = TestDirectory::create();
        fs::create_dir(directory.path().join("sounds")).unwrap();
        fs::write(directory.path().join("sounds/piano.sf2"), b"test").unwrap();
        let path = write_manifest(&directory, &manifest("sounds/piano.sf2"));

        let pack = AssetPack::load(&path).unwrap();

        assert_eq!(pack.manifest().id, "test-piano");
        assert_eq!(pack.manifest_path(), path.canonicalize().unwrap());
        assert_eq!(pack.entry_path(), directory.path().join("sounds/piano.sf2"));
    }

    #[test]
    fn rejects_entry_path_traversal_before_reading_the_entry() {
        let directory = TestDirectory::create();
        let path = write_manifest(&directory, &manifest("../piano.sf2"));

        assert!(matches!(
            AssetPack::load(path),
            Err(AssetPackError::InvalidEntryPath(_))
        ));
    }

    #[test]
    fn rejects_missing_license_metadata() {
        let directory = TestDirectory::create();
        let mut value = manifest("piano.sf2");
        value.license.attribution = "  ".to_owned();
        let path = write_manifest(&directory, &value);

        assert!(matches!(
            AssetPack::load(path),
            Err(AssetPackError::InvalidField {
                field: "license.attribution",
                ..
            })
        ));
    }

    #[test]
    fn rejects_an_entry_extension_that_does_not_match_the_engine() {
        let directory = TestDirectory::create();
        let path = write_manifest(&directory, &manifest("piano.sfz"));

        assert!(matches!(
            AssetPack::load(path),
            Err(AssetPackError::EntryEngineMismatch { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_that_escapes_the_pack_directory() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::create();
        let outside = directory.path().with_extension("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("piano.sf2"), b"test").unwrap();
        symlink(
            outside.join("piano.sf2"),
            directory.path().join("piano.sf2"),
        )
        .unwrap();
        let path = write_manifest(&directory, &manifest("piano.sf2"));

        assert!(matches!(
            AssetPack::load(path),
            Err(AssetPackError::EntryEscapesPack { .. })
        ));
        let _ = fs::remove_dir_all(outside);
    }
}
