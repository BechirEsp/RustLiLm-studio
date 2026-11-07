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
}

pub fn save_asset(
    dir: &Path,
    bytes: &[u8],
    extension: &str,
    kind: MediaKind,
    model: &str,
    prompt: &str,
    project: &str,
) -> Result<Asset> {
    let id = Uuid::new_v4();
    let path = dir.join(format!(
        "{}-{id}.{extension}",
        Local::now().format("%H%M%S")
    ));
    // create_new prevents overwriting any existing output, even across processes.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    let asset = Asset {
        id,
        path,
        kind,
        model: model.into(),
        prompt: prompt.into(),
        project: project.into(),
        created: Utc::now(),
    };
    fs::write(
        asset.path.with_extension("json"),
        serde_json::to_vec_pretty(&asset)?,
    )?;
    Ok(asset)
}

// A per-output-root index makes the gallery survive restarts without walking a large tree.
pub fn load_gallery(root: &Path) -> Result<Vec<Asset>> {
    let path = root.join(".rustlilm-gallery.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let assets: Vec<Asset> = serde_json::from_slice(&fs::read(path)?)?;
    Ok(assets.into_iter().filter(|a| a.path.is_file()).collect())
}

pub fn save_gallery(root: &Path, assets: &[Asset]) -> Result<()> {
    fs::create_dir_all(root)?;
    let temp = root.join(format!(".rustlilm-{}.tmp", Uuid::new_v4()));
    fs::write(&temp, serde_json::to_vec_pretty(assets)?)?;
    let dest = root.join(".rustlilm-gallery.json");
    // Windows rename cannot replace an existing file.
    if dest.exists() {
        fs::remove_file(&dest)?;
    }
    fs::rename(temp, dest)?;
    Ok(())
}
