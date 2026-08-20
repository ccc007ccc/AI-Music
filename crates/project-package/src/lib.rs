//! Durable, directory-based project packages.
//!
//! A package keeps the renderable [`music_core::Project`] separate from
//! generated artifacts and licensed source assets. Adapters should use this
//! module instead of inventing their own project paths.

use fs2::FileExt;
use music_core::Project;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

pub const PACKAGE_EXTENSION: &str = "aimusic";
pub const PACKAGE_FORMAT: &str = "aimusic";
pub const PACKAGE_SCHEMA_VERSION: u32 = 1;
pub const MANIFEST_FILE: &str = "manifest.json";
pub const PROJECT_FILE: &str = "project.json";

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Metadata used to identify a package without loading its musical content.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectManifest {
    pub format: String,
    pub schema_version: u32,
    pub project_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub source_assets: BTreeMap<String, SourceAssetReference>,
}

impl ProjectManifest {
    fn new(name: String, source_assets: BTreeMap<String, SourceAssetReference>) -> Self {
        Self {
            format: PACKAGE_FORMAT.to_owned(),
            schema_version: PACKAGE_SCHEMA_VERSION,
            project_id: music_core::new_id("project"),
            name,
            source_assets,
        }
    }

    fn validate(&self) -> Result<(), ProjectPackageError> {
        if self.format != PACKAGE_FORMAT {
            return Err(ProjectPackageError::UnsupportedFormat(self.format.clone()));
        }
        if self.schema_version != PACKAGE_SCHEMA_VERSION {
            return Err(ProjectPackageError::UnsupportedSchema(self.schema_version));
        }
        validate_text("project id", &self.project_id)?;
        validate_display_name(&self.name)?;
        for (role, asset) in &self.source_assets {
            validate_asset_role(role)?;
            asset.validate()?;
        }
        Ok(())
    }
}

/// A licensed, non-generated resource needed to reproduce project rendering.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceAssetReference {
    pub asset_id: String,
    pub name: String,
    pub location: SourceAssetLocation,
    pub license_source: String,
    pub attribution: String,
}

impl SourceAssetReference {
    fn validate(&self) -> Result<(), ProjectPackageError> {
        validate_text("source asset id", &self.asset_id)?;
        validate_text("source asset name", &self.name)?;
        validate_text("source asset license source", &self.license_source)?;
        validate_text("source asset attribution", &self.attribution)?;
        match &self.location {
            SourceAssetLocation::External { manifest_path } => {
                validate_text("source asset manifest path", manifest_path)?;
                if !Path::new(manifest_path).is_absolute() {
                    return Err(ProjectPackageError::InvalidSourceAssetPath(
                        manifest_path.clone(),
                    ));
                }
                Ok(())
            }
            SourceAssetLocation::Package { manifest_path } => {
                validate_package_asset_path(Path::new(manifest_path))
            }
        }
    }
}

/// Asset manifests may be installed globally or carried inside one project.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceAssetLocation {
    External { manifest_path: String },
    Package { manifest_path: String },
}

/// Fixed artifact areas inside a package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactDirectory {
    Assets,
    Exports,
    Renders,
    History,
}

/// One derived file committed together with the authoritative Project.
#[derive(Clone, Copy, Debug)]
pub struct ArtifactWrite<'a> {
    pub directory: ArtifactDirectory,
    pub filename: &'a str,
    pub bytes: &'a [u8],
}

impl ArtifactDirectory {
    pub const ALL: [Self; 4] = [Self::Assets, Self::Exports, Self::Renders, Self::History];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Assets => "assets",
            Self::Exports => "exports",
            Self::Renders => "renders",
            Self::History => "history",
        }
    }
}

/// An opened `.aimusic` directory bundle.
#[derive(Clone, Debug)]
pub struct ProjectPackage {
    root: PathBuf,
    manifest: ProjectManifest,
}

impl ProjectPackage {
    /// Create a package named `<name>.aimusic` below `parent`.
    pub fn create(
        parent: impl AsRef<Path>,
        name: &str,
        project: &Project,
    ) -> Result<Self, ProjectPackageError> {
        Self::create_with_source_assets(parent, name, project, BTreeMap::new())
    }

    /// Create a package with an explicit set of render source bindings.
    pub fn create_with_source_assets(
        parent: impl AsRef<Path>,
        name: &str,
        project: &Project,
        source_assets: BTreeMap<String, SourceAssetReference>,
    ) -> Result<Self, ProjectPackageError> {
        let name = normalize_display_name(name)?;
        let root = parent.as_ref().join(package_directory_name(&name));
        Self::create_at_with_source_assets(root, name, project, source_assets)
    }

    /// Create a package at an explicitly selected directory.
    pub fn create_at(
        root: impl AsRef<Path>,
        name: impl Into<String>,
        project: &Project,
    ) -> Result<Self, ProjectPackageError> {
        Self::create_at_with_source_assets(root, name, project, BTreeMap::new())
    }

    fn create_at_with_source_assets(
        root: impl AsRef<Path>,
        name: impl Into<String>,
        project: &Project,
        source_assets: BTreeMap<String, SourceAssetReference>,
    ) -> Result<Self, ProjectPackageError> {
        let root = root.as_ref().to_path_buf();
        let name = normalize_display_name(&name.into())?;
        for (role, asset) in &source_assets {
            validate_asset_role(role)?;
            asset.validate()?;
        }
        if root.exists() {
            return Err(ProjectPackageError::AlreadyExists(root));
        }
        if root.file_name().is_none() {
            return Err(ProjectPackageError::InvalidPath(root));
        }
        fs::create_dir_all(&root).map_err(ProjectPackageError::Io)?;
        for directory in ArtifactDirectory::ALL {
            if let Err(error) = fs::create_dir_all(root.join(directory.name())) {
                let _ = fs::remove_dir_all(&root);
                return Err(ProjectPackageError::Io(error));
            }
        }

        let manifest = ProjectManifest::new(name, source_assets);
        let package = Self { root, manifest };
        if let Err(error) = package.write_manifest().and_then(|_| package.save(project)) {
            // The target did not exist before this call, so removing this
            // newly-created bundle cannot discard user data.
            let _ = fs::remove_dir_all(&package.root);
            return Err(error);
        }
        Ok(package)
    }

    /// Open an existing package and load its authoritative project.
    pub fn open(path: impl AsRef<Path>) -> Result<(Self, Project), ProjectPackageError> {
        let root = path.as_ref().to_path_buf();
        reject_symlink(&root)?;
        if !root.is_dir() {
            return Err(ProjectPackageError::NotPackage(root));
        }
        reject_symlink(&root.join(MANIFEST_FILE))?;
        let manifest_text =
            fs::read_to_string(root.join(MANIFEST_FILE)).map_err(ProjectPackageError::Io)?;
        let manifest: ProjectManifest = serde_json::from_str(&manifest_text)
            .map_err(ProjectPackageError::ManifestSerialization)?;
        manifest.validate()?;
        reject_symlink(&root.join(PROJECT_FILE))?;
        let project =
            Project::load(&root.join(PROJECT_FILE)).map_err(ProjectPackageError::Project)?;
        for directory in ArtifactDirectory::ALL {
            let path = root.join(directory.name());
            reject_symlink(&path)?;
            if !path.is_dir() {
                return Err(ProjectPackageError::MissingArtifactDirectory(path));
            }
        }
        Ok((Self { root, manifest }, project))
    }

    /// Save only the renderable source of truth.  The write is atomic with
    /// respect to readers opening the package concurrently.
    pub fn save(&self, project: &Project) -> Result<(), ProjectPackageError> {
        self.ensure_root()?;
        let lock = PackageWriteLock::acquire(&self.root)?;
        let result = self.save_project_inner(project, None);
        drop(lock);
        result
    }

    /// Save the renderable source only when the authoritative Project still
    /// has `expected_revision`.
    pub fn save_if_revision(
        &self,
        expected_revision: u64,
        project: &Project,
    ) -> Result<(), ProjectPackageError> {
        self.ensure_root()?;
        let lock = PackageWriteLock::acquire(&self.root)?;
        let result = self.save_project_inner(project, Some(expected_revision));
        drop(lock);
        result
    }

    fn save_project_inner(
        &self,
        project: &Project,
        expected_revision: Option<u64>,
    ) -> Result<(), ProjectPackageError> {
        self.verify_expected_revision(expected_revision)?;
        let json = project
            .to_pretty_json()
            .map_err(ProjectPackageError::Project)?;
        atomic_write_bytes(&self.project_path(), json.as_bytes())
            .map_err(ProjectPackageError::ProjectIo)
    }

    /// Persist a Project and its derived artifacts as one adapter transaction.
    ///
    /// Every artifact is validated and snapshotted before writing. Artifacts
    /// are written atomically, the authoritative Project is committed last,
    /// and prior artifact contents are restored if any write fails.
    pub fn save_with_artifacts(
        &self,
        project: &Project,
        artifacts: &[ArtifactWrite<'_>],
    ) -> Result<Vec<PathBuf>, ProjectPackageError> {
        self.ensure_root()?;
        let lock = PackageWriteLock::acquire(&self.root)?;
        let result = self.save_with_artifacts_inner(project, artifacts, None);
        drop(lock);
        result
    }

    /// Persist a Project and derived artifacts only when the authoritative
    /// on-disk Project still has `expected_revision`.
    ///
    /// This is the package write seam for long-running or multi-process work:
    /// callers load a revision, compute against it, then compare-and-swap the
    /// complete result. A concurrent writer is reported instead of being
    /// silently overwritten.
    pub fn save_with_artifacts_if_revision(
        &self,
        expected_revision: u64,
        project: &Project,
        artifacts: &[ArtifactWrite<'_>],
    ) -> Result<Vec<PathBuf>, ProjectPackageError> {
        self.ensure_root()?;
        let lock = PackageWriteLock::acquire(&self.root)?;
        let result = self.save_with_artifacts_inner(project, artifacts, Some(expected_revision));
        drop(lock);
        result
    }

    fn save_with_artifacts_inner(
        &self,
        project: &Project,
        artifacts: &[ArtifactWrite<'_>],
        expected_revision: Option<u64>,
    ) -> Result<Vec<PathBuf>, ProjectPackageError> {
        self.ensure_root()?;
        self.verify_expected_revision(expected_revision)?;
        let project_json = project
            .to_pretty_json()
            .map_err(ProjectPackageError::Project)?;
        let mut seen = BTreeSet::new();
        let mut prepared = Vec::with_capacity(artifacts.len());
        for artifact in artifacts {
            let path = self.artifact_path(artifact.directory, artifact.filename)?;
            if !seen.insert(path.clone()) {
                return Err(ProjectPackageError::DuplicateArtifactTarget(path));
            }
            let previous = match fs::read(&path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(ProjectPackageError::ArtifactIo(error)),
            };
            prepared.push(PreparedArtifact {
                path,
                previous,
                bytes: artifact.bytes,
            });
        }

        for (index, artifact) in prepared.iter().enumerate() {
            if let Err(error) = atomic_write_bytes(&artifact.path, artifact.bytes) {
                rollback_artifacts(&prepared[..index], error.to_string())?;
                return Err(ProjectPackageError::ArtifactIo(error));
            }
        }
        if let Err(error) = atomic_write_bytes(&self.project_path(), project_json.as_bytes()) {
            rollback_artifacts(&prepared, error.to_string())?;
            return Err(ProjectPackageError::ProjectIo(error));
        }

        Ok(prepared.into_iter().map(|artifact| artifact.path).collect())
    }

    fn verify_expected_revision(
        &self,
        expected_revision: Option<u64>,
    ) -> Result<(), ProjectPackageError> {
        let Some(expected_revision) = expected_revision else {
            return Ok(());
        };
        let current = Project::load(&self.project_path()).map_err(ProjectPackageError::Project)?;
        if current.revision != expected_revision {
            return Err(ProjectPackageError::RevisionConflict {
                expected: expected_revision,
                actual: current.revision,
            });
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifest(&self) -> &ProjectManifest {
        &self.manifest
    }

    pub fn project_path(&self) -> PathBuf {
        self.root.join(PROJECT_FILE)
    }

    pub fn artifact_dir(&self, directory: ArtifactDirectory) -> PathBuf {
        self.root.join(directory.name())
    }

    pub fn source_asset(&self, role: &str) -> Option<&SourceAssetReference> {
        self.manifest.source_assets.get(role)
    }

    /// Atomically persist one source binding and update the open handle only
    /// after the manifest write succeeds.
    pub fn set_source_asset(
        &mut self,
        role: impl Into<String>,
        asset: SourceAssetReference,
    ) -> Result<(), ProjectPackageError> {
        let role = role.into();
        validate_asset_role(&role)?;
        asset.validate()?;
        self.ensure_root()?;
        let mut manifest = self.manifest.clone();
        manifest.source_assets.insert(role, asset);
        atomic_write_json(&self.root.join(MANIFEST_FILE), &manifest)
            .map_err(ProjectPackageError::Manifest)?;
        self.manifest = manifest;
        Ok(())
    }

    pub fn remove_source_asset(&mut self, role: &str) -> Result<(), ProjectPackageError> {
        validate_asset_role(role)?;
        self.ensure_root()?;
        let mut manifest = self.manifest.clone();
        manifest.source_assets.remove(role);
        atomic_write_json(&self.root.join(MANIFEST_FILE), &manifest)
            .map_err(ProjectPackageError::Manifest)?;
        self.manifest = manifest;
        Ok(())
    }

    /// Resolve a source asset manifest according to its stored portability
    /// mode. Package-relative references may never escape the bundle.
    pub fn resolve_source_asset(&self, role: &str) -> Result<Option<PathBuf>, ProjectPackageError> {
        validate_asset_role(role)?;
        let Some(asset) = self.source_asset(role) else {
            return Ok(None);
        };
        let requested = match &asset.location {
            SourceAssetLocation::External { manifest_path } => PathBuf::from(manifest_path),
            SourceAssetLocation::Package { manifest_path } => {
                let relative = Path::new(manifest_path);
                validate_package_asset_path(relative)?;
                self.root.join(relative)
            }
        };
        let resolved = fs::canonicalize(&requested).map_err(|source| {
            ProjectPackageError::ReadSourceAsset {
                role: role.to_owned(),
                path: requested.clone(),
                source,
            }
        })?;
        let canonical_root = fs::canonicalize(&self.root).map_err(ProjectPackageError::Io)?;
        if matches!(&asset.location, SourceAssetLocation::Package { .. })
            && !resolved.starts_with(&canonical_root)
        {
            return Err(ProjectPackageError::SourceAssetEscapesPackage {
                role: role.to_owned(),
                path: resolved,
            });
        }
        Ok(Some(resolved))
    }

    /// Create a new package from this package's current project state.
    ///
    /// Source bindings and package-local assets move together. If copying an
    /// asset fails, the newly-created target is removed and this package is
    /// left untouched.
    pub fn duplicate(
        &self,
        parent: impl AsRef<Path>,
        name: &str,
        project: &Project,
    ) -> Result<Self, ProjectPackageError> {
        self.ensure_root()?;
        for (role, asset) in &self.manifest.source_assets {
            if matches!(&asset.location, SourceAssetLocation::Package { .. }) {
                self.resolve_source_asset(role)?;
            }
        }
        let target = Self::create_with_source_assets(
            parent,
            name,
            project,
            self.manifest.source_assets.clone(),
        )?;
        let result = copy_package_assets(self, &target);
        if let Err(error) = result {
            // The target did not exist before `create_with_source_assets`, so
            // this rollback cannot remove pre-existing user data.
            let _ = fs::remove_dir_all(&target.root);
            return Err(error);
        }
        Ok(target)
    }

    /// Resolve a file inside a fixed artifact directory.  The filename is
    /// deliberately restricted to one normal path component so an AI or an
    /// imported manifest cannot escape the package root.
    pub fn artifact_path(
        &self,
        directory: ArtifactDirectory,
        filename: &str,
    ) -> Result<PathBuf, ProjectPackageError> {
        validate_artifact_filename(filename)?;
        self.ensure_root()?;
        let directory_path = self.artifact_dir(directory);
        reject_symlink(&directory_path)?;
        if !directory_path.is_dir() {
            return Err(ProjectPackageError::MissingArtifactDirectory(
                directory_path,
            ));
        }
        let path = directory_path.join(filename);
        reject_symlink(&path)?;
        Ok(path)
    }

    /// Write a derived artifact into one of the fixed package directories.
    /// The caller supplies already-serialized bytes so MIDI/audio writers can
    /// use the same path policy as JSON history records.
    pub fn write_artifact(
        &self,
        directory: ArtifactDirectory,
        filename: &str,
        bytes: &[u8],
    ) -> Result<PathBuf, ProjectPackageError> {
        let path = self.artifact_path(directory, filename)?;
        atomic_write_bytes(&path, bytes).map_err(ProjectPackageError::ArtifactIo)?;
        Ok(path)
    }

    /// Serialize a history or metadata record with the package's stable JSON
    /// formatting and write it atomically.
    pub fn write_json_artifact<T: Serialize>(
        &self,
        directory: ArtifactDirectory,
        filename: &str,
        value: &T,
    ) -> Result<PathBuf, ProjectPackageError> {
        let bytes =
            serde_json::to_vec_pretty(value).map_err(ProjectPackageError::ArtifactSerialization)?;
        self.write_artifact(directory, filename, &bytes)
    }

    /// Read a JSON artifact while keeping path validation in one place.
    pub fn read_json_artifact<T: DeserializeOwned>(
        &self,
        directory: ArtifactDirectory,
        filename: &str,
    ) -> Result<T, ProjectPackageError> {
        let path = self.artifact_path(directory, filename)?;
        let text = fs::read_to_string(path).map_err(ProjectPackageError::ArtifactIo)?;
        serde_json::from_str(&text).map_err(ProjectPackageError::ArtifactSerialization)
    }

    fn write_manifest(&self) -> Result<(), ProjectPackageError> {
        atomic_write_json(&self.root.join(MANIFEST_FILE), &self.manifest)
            .map_err(ProjectPackageError::Manifest)
    }

    fn ensure_root(&self) -> Result<(), ProjectPackageError> {
        reject_symlink(&self.root)?;
        if !self.root.is_dir() {
            return Err(ProjectPackageError::NotPackage(self.root.clone()));
        }
        reject_symlink(&self.root.join(MANIFEST_FILE))?;
        if !self.root.join(MANIFEST_FILE).is_file() {
            return Err(ProjectPackageError::NotPackage(self.root.clone()));
        }
        Ok(())
    }
}

fn package_directory_name(name: &str) -> String {
    if name
        .to_ascii_lowercase()
        .ends_with(&format!(".{PACKAGE_EXTENSION}"))
    {
        name.to_owned()
    } else {
        format!("{name}.{PACKAGE_EXTENSION}")
    }
}

fn normalize_display_name(name: &str) -> Result<String, ProjectPackageError> {
    let name = name.trim();
    validate_display_name(name)?;
    let suffix = format!(".{PACKAGE_EXTENSION}");
    Ok(name
        .strip_suffix(&suffix)
        .or_else(|| name.strip_suffix(&suffix.to_ascii_uppercase()))
        .unwrap_or(name)
        .trim()
        .to_owned())
}

fn validate_display_name(name: &str) -> Result<(), ProjectPackageError> {
    validate_text("project name", name)?;
    if name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(ProjectPackageError::InvalidName(name.to_owned()));
    }
    Ok(())
}

fn validate_text(field: &str, value: &str) -> Result<(), ProjectPackageError> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(ProjectPackageError::InvalidText {
            field: field.to_owned(),
        });
    }
    Ok(())
}

fn validate_artifact_filename(filename: &str) -> Result<(), ProjectPackageError> {
    if filename.trim().is_empty() || filename == "." || filename == ".." {
        return Err(ProjectPackageError::InvalidArtifactFilename(
            filename.to_owned(),
        ));
    }
    let mut components = Path::new(filename).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err(ProjectPackageError::InvalidArtifactFilename(
            filename.to_owned(),
        )),
    }
}

fn validate_asset_role(role: &str) -> Result<(), ProjectPackageError> {
    if role.is_empty()
        || !role
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b'_' | b'-'))
    {
        return Err(ProjectPackageError::InvalidAssetRole(role.to_owned()));
    }
    Ok(())
}

fn validate_package_asset_path(path: &Path) -> Result<(), ProjectPackageError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
        || path.components().next() != Some(Component::Normal("assets".as_ref()))
    {
        return Err(ProjectPackageError::InvalidSourceAssetPath(
            path.to_string_lossy().into_owned(),
        ));
    }
    Ok(())
}

fn reject_symlink(path: &Path) -> Result<(), ProjectPackageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(ProjectPackageError::Symlink(path.to_path_buf()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ProjectPackageError::Io(error)),
    }
}

fn copy_directory_contents(source: &Path, destination: &Path) -> Result<(), ProjectPackageError> {
    let entries = fs::read_dir(source).map_err(ProjectPackageError::Io)?;
    for entry in entries {
        let entry = entry.map_err(ProjectPackageError::Io)?;
        let source_path = entry.path();
        let name = entry.file_name();
        let destination_path = destination.join(name);
        let metadata = fs::symlink_metadata(&source_path).map_err(ProjectPackageError::Io)?;
        if metadata.file_type().is_symlink() {
            return Err(ProjectPackageError::Symlink(source_path));
        }
        if metadata.is_dir() {
            fs::create_dir_all(&destination_path).map_err(ProjectPackageError::Io)?;
            copy_directory_contents(&source_path, &destination_path)?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &destination_path).map_err(|source| {
                ProjectPackageError::AssetCopy {
                    from: source_path.clone(),
                    to: destination_path.clone(),
                    error: source,
                }
            })?;
        } else {
            return Err(ProjectPackageError::AssetCopy {
                from: source_path,
                to: destination_path,
                error: std::io::Error::other("unsupported asset file type"),
            });
        }
    }
    Ok(())
}

fn copy_package_assets(
    source: &ProjectPackage,
    destination: &ProjectPackage,
) -> Result<(), ProjectPackageError> {
    source.ensure_root()?;
    destination.ensure_root()?;
    let source = source.artifact_dir(ArtifactDirectory::Assets);
    let destination = destination.artifact_dir(ArtifactDirectory::Assets);
    reject_symlink(&source)?;
    reject_symlink(&destination)?;
    copy_directory_contents(&source, &destination)
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), std::io::Error> {
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    atomic_write_bytes(path, &bytes)
}

fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("path has no parent"))?;
    fs::create_dir_all(parent)?;
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file"),
        std::process::id(),
        counter
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok::<(), std::io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

const WRITE_LOCK_FILE: &str = ".aimusic-write.lock";

struct PackageWriteLock {
    file: fs::File,
}

impl PackageWriteLock {
    fn acquire(root: &Path) -> Result<Self, ProjectPackageError> {
        let path = root.join(WRITE_LOCK_FILE);
        reject_symlink(&path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(ProjectPackageError::Io)?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Self { file }),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                Err(ProjectPackageError::WriteInProgress(path))
            }
            Err(error) => Err(ProjectPackageError::Io(error)),
        }
    }
}

impl Drop for PackageWriteLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

struct PreparedArtifact<'a> {
    path: PathBuf,
    previous: Option<Vec<u8>>,
    bytes: &'a [u8],
}

fn rollback_artifacts(
    artifacts: &[PreparedArtifact<'_>],
    original: String,
) -> Result<(), ProjectPackageError> {
    for artifact in artifacts.iter().rev() {
        let result = if let Some(previous) = &artifact.previous {
            atomic_write_bytes(&artifact.path, previous)
        } else {
            match fs::remove_file(&artifact.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            }
        };
        if let Err(rollback) = result {
            return Err(ProjectPackageError::ArtifactRollback { original, rollback });
        }
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ProjectPackageError {
    #[error("path is not an AI Music package directory: {0}")]
    NotPackage(PathBuf),
    #[error("package already exists: {0}")]
    AlreadyExists(PathBuf),
    #[error("invalid package path: {0}")]
    InvalidPath(PathBuf),
    #[error("invalid project name: {0}")]
    InvalidName(String),
    #[error("invalid {field}")]
    InvalidText { field: String },
    #[error("invalid artifact filename: {0}")]
    InvalidArtifactFilename(String),
    #[error("duplicate artifact target in one transaction: {0}")]
    DuplicateArtifactTarget(PathBuf),
    #[error("package revision changed before commit (expected {expected}, found {actual})")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("another package write is already in progress: {0}")]
    WriteInProgress(PathBuf),
    #[error("invalid source asset role: {0}")]
    InvalidAssetRole(String),
    #[error(
        "invalid source asset manifest path (external paths must be absolute; package paths must stay below assets/): {0}"
    )]
    InvalidSourceAssetPath(String),
    #[error("could not resolve source asset '{role}' at {path:?}: {source}")]
    ReadSourceAsset {
        role: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("source asset '{role}' resolves outside the package: {path:?}")]
    SourceAssetEscapesPackage { role: String, path: PathBuf },
    #[error("could not copy package asset {from:?} to {to:?}: {error}")]
    AssetCopy {
        from: PathBuf,
        to: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("unsupported package format: {0}")]
    UnsupportedFormat(String),
    #[error("unsupported package schema version: {0}")]
    UnsupportedSchema(u32),
    #[error("missing artifact directory: {0}")]
    MissingArtifactDirectory(PathBuf),
    #[error("symbolic links are not allowed for package roots or fixed directories: {0}")]
    Symlink(PathBuf),
    #[error("manifest I/O failed: {0}")]
    Manifest(#[source] std::io::Error),
    #[error("manifest serialization failed: {0}")]
    ManifestSerialization(#[source] serde_json::Error),
    #[error("project I/O or serialization failed: {0}")]
    Project(#[source] music_core::ProjectError),
    #[error("project file write failed: {0}")]
    ProjectIo(#[source] std::io::Error),
    #[error("artifact I/O failed: {0}")]
    ArtifactIo(#[source] std::io::Error),
    #[error("artifact transaction failed ({original}); rollback also failed: {rollback}")]
    ArtifactRollback {
        original: String,
        #[source]
        rollback: std::io::Error,
    },
    #[error("artifact serialization failed: {0}")]
    ArtifactSerialization(#[source] serde_json::Error),
    #[error("I/O failed: {0}")]
    Io(#[source] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("aimusic-package-{label}-{stamp}"))
    }

    fn external_asset(path: &Path) -> SourceAssetReference {
        SourceAssetReference {
            asset_id: "test-piano".to_owned(),
            name: "Test Piano".to_owned(),
            location: SourceAssetLocation::External {
                manifest_path: path.to_string_lossy().into_owned(),
            },
            license_source: "https://example.test/license".to_owned(),
            attribution: "Test Piano authors".to_owned(),
        }
    }

    #[test]
    fn creates_and_reopens_complete_bundle() {
        let parent = temp_root("create");
        fs::create_dir_all(&parent).expect("parent");
        let project = Project::demo();
        let package = ProjectPackage::create(&parent, "First Light", &project).expect("create");
        assert_eq!(package.root().file_name().unwrap(), "First Light.aimusic");
        assert!(package.artifact_dir(ArtifactDirectory::Renders).is_dir());

        let (opened, loaded) = ProjectPackage::open(package.root()).expect("open");
        assert_eq!(opened.manifest().name, "First Light");
        assert_eq!(loaded, project);
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn saves_project_and_artifacts_together() {
        let parent = temp_root("transaction");
        fs::create_dir_all(&parent).expect("parent");
        let package =
            ProjectPackage::create(&parent, "Transaction", &Project::default()).expect("create");
        let project = Project {
            revision: 1,
            ..Project::default()
        };
        let memory = b"memory";
        let render = b"RIFF-render";

        let paths = package
            .save_with_artifacts(
                &project,
                &[
                    ArtifactWrite {
                        directory: ArtifactDirectory::History,
                        filename: "memory.json",
                        bytes: memory,
                    },
                    ArtifactWrite {
                        directory: ArtifactDirectory::Renders,
                        filename: "result.wav",
                        bytes: render,
                    },
                ],
            )
            .expect("transaction");

        assert_eq!(paths.len(), 2);
        assert_eq!(ProjectPackage::open(package.root()).unwrap().1, project);
        assert_eq!(fs::read(&paths[0]).unwrap(), memory);
        assert_eq!(fs::read(&paths[1]).unwrap(), render);
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn compare_and_swap_rejects_a_stale_package_writer() {
        let parent = temp_root("revision-conflict");
        fs::create_dir_all(&parent).expect("parent");
        let package =
            ProjectPackage::create(&parent, "Conflict", &Project::default()).expect("create");
        let revision_one = Project {
            revision: 1,
            ..Project::default()
        };
        package.save(&revision_one).expect("concurrent save");
        let stale_result = package.save_with_artifacts_if_revision(
            0,
            &Project {
                revision: 2,
                ..Project::default()
            },
            &[ArtifactWrite {
                directory: ArtifactDirectory::History,
                filename: "memory.json",
                bytes: b"stale",
            }],
        );

        assert!(matches!(
            stale_result,
            Err(ProjectPackageError::RevisionConflict {
                expected: 0,
                actual: 1
            })
        ));
        assert_eq!(ProjectPackage::open(package.root()).unwrap().1.revision, 1);
        assert!(
            !package
                .artifact_path(ArtifactDirectory::History, "memory.json")
                .unwrap()
                .exists()
        );
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn compare_and_swap_rejects_an_overlapping_package_write() {
        let parent = temp_root("write-lock");
        fs::create_dir_all(&parent).expect("parent");
        let package =
            ProjectPackage::create(&parent, "Locked", &Project::default()).expect("create");
        let lock = PackageWriteLock::acquire(package.root()).expect("lock");

        let result = package.save(&Project {
            revision: 1,
            ..Project::default()
        });

        assert!(matches!(
            result,
            Err(ProjectPackageError::WriteInProgress(_))
        ));
        drop(lock);
        package
            .save_if_revision(0, &Project::default())
            .expect("lock is released when the guard drops");
        let _ = fs::remove_dir_all(parent);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn failed_artifact_transaction_restores_prior_files_and_project() {
        let parent = temp_root("transaction-rollback");
        fs::create_dir_all(&parent).expect("parent");
        let package = ProjectPackage::create(&parent, "Rollback", &Project::default())
            .expect("create package");
        package
            .write_artifact(ArtifactDirectory::History, "memory.json", b"old")
            .expect("old memory");
        let project = Project {
            revision: 1,
            ..Project::default()
        };
        let long_filename = format!("{}.json", "x".repeat(246));

        let result = package.save_with_artifacts(
            &project,
            &[
                ArtifactWrite {
                    directory: ArtifactDirectory::History,
                    filename: "memory.json",
                    bytes: b"new",
                },
                ArtifactWrite {
                    directory: ArtifactDirectory::History,
                    filename: &long_filename,
                    bytes: b"cannot stage because the temporary name exceeds NAME_MAX",
                },
            ],
        );

        assert!(matches!(result, Err(ProjectPackageError::ArtifactIo(_))));
        assert_eq!(
            fs::read(
                package
                    .artifact_path(ArtifactDirectory::History, "memory.json")
                    .unwrap()
            )
            .unwrap(),
            b"old"
        );
        assert_eq!(ProjectPackage::open(package.root()).unwrap().1.revision, 0);
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn rejects_artifact_path_escape() {
        let parent = temp_root("escape");
        fs::create_dir_all(&parent).expect("parent");
        let package = ProjectPackage::create(&parent, "Safe", &Project::default()).expect("create");
        assert!(
            package
                .artifact_path(ArtifactDirectory::Assets, "../project.json")
                .is_err()
        );
        assert!(
            package
                .artifact_path(ArtifactDirectory::Assets, "/tmp/file")
                .is_err()
        );
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn source_asset_bindings_round_trip_and_resolve() {
        let parent = temp_root("source-asset");
        fs::create_dir_all(&parent).expect("parent");
        let asset_manifest = parent.join("piano-pack.json");
        fs::write(&asset_manifest, b"{}").expect("asset manifest");
        let mut source_assets = BTreeMap::new();
        source_assets.insert(
            "instrument:piano".to_owned(),
            external_asset(&asset_manifest),
        );
        let package = ProjectPackage::create_with_source_assets(
            &parent,
            "Bound",
            &Project::default(),
            source_assets,
        )
        .expect("create");

        let (opened, _) = ProjectPackage::open(package.root()).expect("open");
        assert_eq!(
            opened
                .resolve_source_asset("instrument:piano")
                .expect("resolve"),
            Some(asset_manifest.canonicalize().expect("canonical asset"))
        );
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn package_source_assets_cannot_escape_the_bundle() {
        let parent = temp_root("source-escape");
        fs::create_dir_all(&parent).expect("parent");
        let mut source_assets = BTreeMap::new();
        source_assets.insert(
            "instrument:piano".to_owned(),
            SourceAssetReference {
                asset_id: "test-piano".to_owned(),
                name: "Test Piano".to_owned(),
                location: SourceAssetLocation::Package {
                    manifest_path: "../piano-pack.json".to_owned(),
                },
                license_source: "https://example.test/license".to_owned(),
                attribution: "Test Piano authors".to_owned(),
            },
        );
        assert!(matches!(
            ProjectPackage::create_with_source_assets(
                &parent,
                "Unsafe",
                &Project::default(),
                source_assets,
            ),
            Err(ProjectPackageError::InvalidSourceAssetPath(_))
        ));
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn external_source_asset_paths_must_be_absolute() {
        let parent = temp_root("relative-external");
        fs::create_dir_all(&parent).expect("parent");
        let mut source_assets = BTreeMap::new();
        source_assets.insert(
            "instrument:piano".to_owned(),
            external_asset(Path::new("relative/piano-pack.json")),
        );
        assert!(matches!(
            ProjectPackage::create_with_source_assets(
                &parent,
                "Relative",
                &Project::default(),
                source_assets,
            ),
            Err(ProjectPackageError::InvalidSourceAssetPath(_))
        ));
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn copies_package_assets_for_relocated_bundle() {
        let parent = temp_root("copy-assets");
        fs::create_dir_all(&parent).expect("parent");
        let source =
            ProjectPackage::create(&parent, "Source", &Project::default()).expect("create source");
        let source_manifest = source.artifact_dir(ArtifactDirectory::Assets).join("piano");
        fs::create_dir_all(&source_manifest).expect("asset directory");
        let source_file = source_manifest.join("pack.json");
        fs::write(&source_file, b"asset").expect("asset file");
        let mut source = source;
        source
            .set_source_asset(
                "instrument:piano",
                SourceAssetReference {
                    asset_id: "package-piano".to_owned(),
                    name: "Package Piano".to_owned(),
                    location: SourceAssetLocation::Package {
                        manifest_path: "assets/piano/pack.json".to_owned(),
                    },
                    license_source: "https://example.test/license".to_owned(),
                    attribution: "Package Piano authors".to_owned(),
                },
            )
            .expect("bind source asset");

        let destination = source
            .duplicate(&parent, "Destination", &Project::default())
            .expect("duplicate package");
        assert_eq!(
            destination
                .resolve_source_asset("instrument:piano")
                .expect("resolve destination asset")
                .and_then(|path| fs::read_to_string(path).ok()),
            Some("asset".to_owned())
        );
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn resolves_package_asset_when_bundle_was_opened_by_relative_path() {
        let current = std::env::current_dir().expect("current directory");
        let directory_name = temp_root("relative-open")
            .file_name()
            .expect("temporary directory name")
            .to_owned();
        let parent = current.join(&directory_name);
        fs::create_dir_all(&parent).expect("parent");
        let mut package =
            ProjectPackage::create(&parent, "Relative Open", &Project::default()).expect("create");
        let asset = package
            .artifact_dir(ArtifactDirectory::Assets)
            .join("pack.json");
        fs::write(&asset, b"{}").expect("asset file");
        package
            .set_source_asset(
                "instrument:piano",
                SourceAssetReference {
                    asset_id: "relative-piano".to_owned(),
                    name: "Relative Piano".to_owned(),
                    location: SourceAssetLocation::Package {
                        manifest_path: "assets/pack.json".to_owned(),
                    },
                    license_source: "https://example.test/license".to_owned(),
                    attribution: "Relative Piano authors".to_owned(),
                },
            )
            .expect("bind asset");
        let relative = PathBuf::from(&directory_name).join("Relative Open.aimusic");
        let (opened, _) = ProjectPackage::open(relative).expect("open relative package");
        assert_eq!(
            opened
                .resolve_source_asset("instrument:piano")
                .expect("resolve asset"),
            Some(asset.canonicalize().expect("canonical asset"))
        );
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn save_updates_only_project_source() {
        let parent = temp_root("save");
        fs::create_dir_all(&parent).expect("parent");
        let package =
            ProjectPackage::create(&parent, "Saved", &Project::default()).expect("create");
        let project = Project {
            revision: 7,
            ..Project::default()
        };
        package.save(&project).expect("save");
        let (_, loaded) = ProjectPackage::open(package.root()).expect("open");
        assert_eq!(loaded.revision, 7);
        assert_eq!(package.manifest().name, "Saved");
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn writes_and_reads_history_artifact() {
        let parent = temp_root("history");
        fs::create_dir_all(&parent).expect("parent");
        let package =
            ProjectPackage::create(&parent, "History", &Project::default()).expect("create");
        let value = serde_json::json!({"revision": 3, "kind": "critique"});
        package
            .write_json_artifact(ArtifactDirectory::History, "revision-3.json", &value)
            .expect("write");
        let loaded: serde_json::Value = package
            .read_json_artifact(ArtifactDirectory::History, "revision-3.json")
            .expect("read");
        assert_eq!(loaded, value);
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn an_open_handle_does_not_recreate_a_deleted_package() {
        let parent = temp_root("deleted");
        fs::create_dir_all(&parent).expect("parent");
        let package =
            ProjectPackage::create(&parent, "Deleted", &Project::default()).expect("create");
        fs::remove_dir_all(package.root()).expect("remove package");
        assert!(matches!(
            package.save(&Project::default()),
            Err(ProjectPackageError::NotPackage(_))
        ));
        assert!(matches!(
            package.artifact_path(ArtifactDirectory::Renders, "preview.wav"),
            Err(ProjectPackageError::NotPackage(_))
        ));
        assert!(!package.root().exists());
        let _ = fs::remove_dir_all(parent);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_artifact_directory() {
        use std::os::unix::fs::symlink;

        let parent = temp_root("symlink");
        fs::create_dir_all(&parent).expect("parent");
        let package =
            ProjectPackage::create(&parent, "Symlink", &Project::default()).expect("create");
        let outside = parent.join("outside");
        fs::create_dir_all(&outside).expect("outside");
        let renders = package.artifact_dir(ArtifactDirectory::Renders);
        fs::remove_dir(&renders).expect("remove renders");
        symlink(&outside, &renders).expect("symlink");
        assert!(matches!(
            ProjectPackage::open(package.root()),
            Err(ProjectPackageError::Symlink(_))
        ));
        let _ = fs::remove_dir_all(parent);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_artifact_file() {
        use std::os::unix::fs::symlink;

        let parent = temp_root("artifact-symlink");
        fs::create_dir_all(&parent).expect("parent");
        let package =
            ProjectPackage::create(&parent, "Artifact", &Project::default()).expect("create");
        let outside = parent.join("outside.json");
        fs::write(&outside, b"{}").expect("outside");
        let linked = package
            .artifact_dir(ArtifactDirectory::History)
            .join("linked.json");
        symlink(&outside, &linked).expect("symlink");
        assert!(matches!(
            package
                .read_json_artifact::<serde_json::Value>(ArtifactDirectory::History, "linked.json"),
            Err(ProjectPackageError::Symlink(_))
        ));
        let _ = fs::remove_dir_all(parent);
    }
}
