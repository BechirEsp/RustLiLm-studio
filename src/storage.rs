use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    #[default]
    LiteLlm,
    Ollama,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Self::LiteLlm => "LiteLLM",
            Self::Ollama => "Ollama",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub provider: Provider,
    pub remember_key: bool,
    pub gateway_url: String,
    pub ollama_url: String,
    pub output: PathBuf,
    pub project_folders: bool,
    pub date_folders: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: Provider::LiteLlm,
            remember_key: true,
            gateway_url: "http://localhost:4000".into(),
            ollama_url: "http://localhost:11434".into(),
            output: directories::UserDirs::new()
                .and_then(|d| d.picture_dir().map(Path::to_path_buf))
                .unwrap_or_else(|| PathBuf::from("."))
                .join("RustLiLm"),
            project_folders: true,
            date_folders: true,
        }
    }
}

pub fn prepare_reference(source: &Path, logs: &Path) -> Result<PathBuf> {
    if fs::metadata(source)?.len() > 100 * 1024 * 1024 {
        bail!("Choose an image smaller than 100 MiB");
    }
    let mut reader = image::ImageReader::open(source)?.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .context("Cannot decode the selected image")?;
    let dir = logs.join("inputs");
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.png", Uuid::new_v4()));
    image.save_with_format(&path, image::ImageFormat::Png)?;
    Ok(path)
}

pub fn config_dir() -> Result<PathBuf> {
    Ok(
        directories::ProjectDirs::from("", "RustLiLm", "RustLiLm Studio")
            .context("No application configuration directory")?
            .config_dir()
            .to_path_buf(),
    )
}

impl Settings {
    pub fn endpoint_url(&self) -> &str {
        match self.provider {
            Provider::LiteLlm => &self.gateway_url,
            Provider::Ollama => &self.ollama_url,
        }
    }
    pub fn load() -> Result<Self> {
        let dir = config_dir()?;
        let database = crate::database::Database::open(&dir.join("settings.sqlite3"))?;
        if let Some(settings) = database.settings()? {
            return Ok(settings);
        }
        let legacy = dir.join("settings.json");
        if legacy.exists() {
            let settings: Self = serde_json::from_slice(&fs::read(&legacy)?)?;
            database.save_settings(&settings)?;
            fs::remove_file(legacy)?;
            return Ok(settings);
        }
        Ok(Self::default())
    }

    pub fn save(&self) -> Result<()> {
        crate::database::Database::open(&config_dir()?.join("settings.sqlite3"))?
            .save_settings(self)
    }

    pub fn destination(&self, project: &str) -> Result<PathBuf> {
        if self.output.as_os_str().is_empty() {
            bail!("Choose an output folder first");
        }
        let mut dir = self.output.clone();
        if self.project_folders {
            dir.push(safe_component(project));
        }
        if self.date_folders {
            dir.push(Local::now().format("%Y-%m-%d").to_string());
        }
        fs::create_dir_all(&dir).context("Cannot create output folder")?;
        Ok(dir)
    }
}

pub fn safe_component(value: &str) -> String {
    let value: String = value
        .trim()
        .chars()
        .take(72)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let value = value.trim_matches('_');
    let stem = value.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if value.is_empty() {
        "Untitled".into()
    } else if reserved.contains(&stem.as_str()) {
        format!("project_{value}")
    } else {
        value.into()
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Asset {
    pub id: Uuid,
    pub path: PathBuf,
    pub kind: MediaKind,
    pub model: String,
    pub prompt: String,
    pub project: String,
    pub created: DateTime<Utc>,
    #[serde(default)]
    pub prompt_id: Option<Uuid>,
}

pub struct AssetDetails<'a> {
    pub kind: MediaKind,
    pub model: &'a str,
    pub prompt: &'a str,
    pub project: &'a str,
    pub prompt_id: Option<Uuid>,
}

pub fn metadata_dir(root: &Path) -> PathBuf {
    root.join("_studio")
}

pub fn save_asset(
    dir: &Path,
    logs: &Path,
    bytes: &[u8],
    extension: &str,
    details: AssetDetails<'_>,
) -> Result<Asset> {
    fs::create_dir_all(dir)?;
    let id = Uuid::new_v4();
    let path = dir.join(format!(
        "{}-{id}.{extension}",
        Local::now().format("%H%M%S")
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    let asset = Asset {
        id,
        path,
        kind: details.kind,
        model: details.model.into(),
        prompt: details.prompt.into(),
        project: details.project.into(),
        prompt_id: details.prompt_id,
        created: Utc::now(),
    };
    crate::database::Database::workspace(logs)?.save_assets(std::slice::from_ref(&asset))?;
    Ok(asset)
}

pub fn load_gallery(root: &Path) -> Result<Vec<Asset>> {
    migrate_legacy(root)?;
    Ok(crate::database::Database::workspace(&metadata_dir(root))?
        .assets()?
        .into_iter()
        .filter(|a| a.path.is_file())
        .collect())
}

pub fn save_gallery(root: &Path, assets: &[Asset]) -> Result<()> {
    crate::database::Database::workspace(&metadata_dir(root))?.save_assets(assets)
}

/// Import v0.1/v0.2 JSON metadata before removing recognized legacy files.
/// Imports are idempotent; files remain recoverable if the process is interrupted.
pub fn migrate_legacy(root: &Path) -> Result<()> {
    let logs = metadata_dir(root);
    let mut db = crate::database::Database::workspace(&logs)?;
    if db.migrated()? {
        return Ok(());
    }
    let mut cleanup = Vec::new();
    let mut assets = db.assets()?;
    for path in [
        root.join(".rustlilm-gallery.json"),
        logs.join("gallery.json"),
    ] {
        if path.exists() {
            let old: Vec<Asset> = serde_json::from_slice(&fs::read(&path)?)
                .context("Cannot import the previous gallery index")?;
            assets.extend(old);
            cleanup.push(path);
        }
    }
    for path in json_files(&logs.join("assets"))? {
        let asset: Asset = serde_json::from_slice(&fs::read(&path)?)?;
        assets.push(asset);
        cleanup.push(path);
    }
    for asset in &assets {
        let path = asset.path.with_extension("json");
        if path.exists() {
            if let Ok(old) = serde_json::from_slice::<Asset>(&fs::read(&path)?) {
                if old.id == asset.id {
                    cleanup.push(path);
                }
            }
        }
    }
    db.save_assets(&assets)?;
    let projects = logs.join("projects.json");
    if projects.exists() {
        let book: crate::projects::ProjectBook = serde_json::from_slice(&fs::read(&projects)?)?;
        db.save_projects(&book)?;
        cleanup.push(projects);
    }
    let mut jobs = json_files(&logs.join("jobs"))?;
    find_legacy_jobs(root, &logs, &mut jobs)?;
    for path in jobs {
        // A generically named unrelated JSON file is not an application checkpoint.
        if let Ok(mut pending) =
            serde_json::from_slice::<crate::gateway::PendingVideo>(&fs::read(&path)?)
        {
            pending.request.logs = logs.clone();
            db.save_job(&pending)?;
            cleanup.push(path);
        }
    }
    cleanup.sort();
    cleanup.dedup();
    for path in cleanup {
        fs::remove_file(path)?;
    }
    let marker = logs.join("migration-v1.done");
    if marker.exists() {
        fs::remove_file(marker)?;
    }
    db.finish_migration()?;
    Ok(())
}

fn json_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.path().extension().is_some_and(|s| s == "json") {
            paths.push(entry.path());
        }
    }
    Ok(paths)
}
fn find_legacy_jobs(dir: &Path, logs: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() && path != logs {
            find_legacy_jobs(&path, logs, paths)?;
        } else if kind.is_file() && entry.file_name() == "pending-video.json" {
            paths.push(path);
        }
    }
    Ok(())
}
