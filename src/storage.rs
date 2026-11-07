use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub gateway_url: String,
    pub output: PathBuf,
    pub project_folders: bool,
    pub date_folders: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            gateway_url: "http://localhost:4000".into(),
            output: directories::UserDirs::new()
                .and_then(|d| d.picture_dir().map(Path::to_path_buf))
                .unwrap_or_else(|| PathBuf::from("."))
                .join("RustLiLm"),
            project_folders: true,
            date_folders: true,
        }
    }
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
    pub fn load() -> Result<Self> {
        let path = config_dir()?.join("settings.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }

    pub fn save(&self) -> Result<()> {
        let dir = config_dir()?;
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("settings.json"), serde_json::to_vec_pretty(self)?)?;
        Ok(())
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

pub fn write_json(path: &Path, value: &(impl Serialize + ?Sized)) -> Result<()> {
    fs::create_dir_all(
        path.parent()
            .context("Metadata file has no parent folder")?,
    )?;
    let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    drop(file);
    #[cfg(target_os = "windows")]
    {
        // Keep a recoverable backup while replacing a file on Windows.
        let backup = path.with_extension("json.bak");
        if path.exists() {
            if backup.exists() {
                fs::remove_file(&backup)?;
            }
            fs::rename(path, &backup)?;
        }
        if let Err(error) = fs::rename(&temp, path) {
            if backup.exists() {
                let _ = fs::rename(&backup, path);
            }
            return Err(error.into());
        }
        if backup.exists() {
            fs::remove_file(backup)?;
        }
    }
    #[cfg(not(target_os = "windows"))]
    fs::rename(temp, path)?;
    Ok(())
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
    write_json(&logs.join("assets").join(format!("{id}.json")), &asset)?;
    Ok(asset)
}

pub fn load_gallery(root: &Path) -> Result<Vec<Asset>> {
    migrate_legacy(root)?;
    let path = metadata_dir(root).join("gallery.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let assets: Vec<Asset> = serde_json::from_slice(&fs::read(path)?)?;
    Ok(assets.into_iter().filter(|a| a.path.is_file()).collect())
}

pub fn save_gallery(root: &Path, assets: &[Asset]) -> Result<()> {
    write_json(&metadata_dir(root).join("gallery.json"), &assets)
}

/// Move only metadata recognized as ours. Unrelated JSON files are never touched.
pub fn migrate_legacy(root: &Path) -> Result<()> {
    let logs = metadata_dir(root);
    if logs.join("migration-v1.done").exists() {
        return Ok(());
    }
    fs::create_dir_all(&logs)?;
    let old_index = root.join(".rustlilm-gallery.json");
    if old_index.exists() {
        let assets: Vec<Asset> = serde_json::from_slice(&fs::read(&old_index)?)
            .context("Cannot migrate the previous gallery index")?;
        for asset in &assets {
            let sidecar = asset.path.with_extension("json");
            if sidecar.exists() {
                if let Ok(old) = serde_json::from_slice::<Asset>(&fs::read(&sidecar)?) {
                    if old.id == asset.id {
                        write_json(
                            &logs.join("assets").join(format!("{}.json", asset.id)),
                            &old,
                        )?;
                        fs::remove_file(sidecar)?;
                    }
                }
            }
        }
        let new_index = logs.join("gallery.json");
        if !new_index.exists() {
            write_json(&new_index, &assets)?;
        }
        fs::remove_file(old_index)?;
    }
    migrate_jobs(root, &logs)?;
    fs::write(logs.join("migration-v1.done"), b"1")?;
    Ok(())
}

fn migrate_jobs(dir: &Path, logs: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() && path != logs {
            migrate_jobs(&path, logs)?;
        } else if kind.is_file() && entry.file_name() == "pending-video.json" {
            if let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&fs::read(&path)?) {
                if value["id"].is_string() && value.pointer("/request/destination").is_some() {
                    let checkpoint = logs.join("jobs").join(format!("{}.json", Uuid::new_v4()));
                    value["request"]["logs"] = serde_json::to_value(logs)?;
                    value["checkpoint"] = serde_json::to_value(&checkpoint)?;
                    write_json(&checkpoint, &value)?;
                    fs::remove_file(path)?;
                }
            }
        }
    }
    Ok(())
}
