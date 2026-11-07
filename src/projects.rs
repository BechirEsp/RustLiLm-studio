use crate::storage::{metadata_dir, safe_component, write_json, Asset, MediaKind};
use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromptEntry {
    pub id: Uuid,
    pub text: String,
    pub model: String,
    pub kind: MediaKind,
    pub size: String,
    pub seconds: u32,
    pub created: DateTime<Utc>,
    pub state: PromptState,
    pub detail: Option<String>,
    #[serde(default)]
    pub imported: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptState {
    Submitted,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub prompts: Vec<PromptEntry>,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct ProjectBook {
    pub projects: Vec<Project>,
}

impl ProjectBook {
    pub fn load(root: &Path, gallery: &[Asset]) -> Result<Self> {
        let path = metadata_dir(root).join("projects.json");
        let mut book: Self = if path.exists() {
            serde_json::from_slice(&fs::read(path)?)?
        } else {
            Self::default()
        };
        book.import_assets(gallery);
        book.save(root)?;
        Ok(book)
    }

    pub fn import_assets(&mut self, gallery: &[Asset]) {
        // Import the previous gallery once, including projects created before prompt history existed.
        for asset in gallery.iter().rev() {
            let index =
                if let Some(index) = self.projects.iter().position(|p| p.name == asset.project) {
                    index
                } else {
                    self.projects.push(Project {
                        id: Uuid::new_v4(),
                        name: asset.project.clone(),
                        prompts: Vec::new(),
                    });
                    self.projects.len() - 1
                };
            let project = &mut self.projects[index];
            let known = project.prompts.iter().position(|p| {
                asset.prompt_id == Some(p.id)
                    || (asset.prompt_id.is_none()
                        && p.imported
                        && p.text == asset.prompt
                        && p.model == asset.model
                        && p.kind == asset.kind)
            });
            if let Some(index) = known {
                project.prompts[index].state = PromptState::Completed;
                project.prompts[index].detail = None;
            } else {
                project.prompts.insert(
                    0,
                    PromptEntry {
                        id: asset.prompt_id.unwrap_or_else(Uuid::new_v4),
                        text: asset.prompt.clone(),
                        model: asset.model.clone(),
                        kind: asset.kind,
                        size: if asset.kind == MediaKind::Image {
                            "1024x1024"
                        } else {
                            "1280x720"
                        }
                        .into(),
                        seconds: 4,
                        created: asset.created,
                        state: PromptState::Completed,
                        detail: None,
                        imported: true,
                    },
                );
            }
        }
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        write_json(&metadata_dir(root).join("projects.json"), self)
    }

    pub fn create(&mut self, name: &str) -> Result<Uuid> {
        let name = name.trim();
        if name.is_empty() {
            bail!("Enter a project name");
        }
        if name.chars().count() > 72 {
            bail!("Use a project name of 72 characters or fewer");
        }
        let folder = safe_component(name).to_ascii_lowercase();
        if self.projects.iter().any(|p| {
            p.name.eq_ignore_ascii_case(name)
                || safe_component(&p.name).to_ascii_lowercase() == folder
        }) {
            bail!("A project with this name or output folder already exists");
        }
        let id = Uuid::new_v4();
        self.projects.push(Project {
            id,
            name: name.into(),
            prompts: Vec::new(),
        });
        Ok(id)
    }

    pub fn project(&self, id: Uuid) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }
    pub fn prompt(&self, id: Uuid) -> Option<&PromptEntry> {
        self.projects
            .iter()
            .flat_map(|p| &p.prompts)
            .find(|p| p.id == id)
    }

    pub fn matches_prompt(&self, asset: &Asset, id: Uuid) -> bool {
        asset.prompt_id == Some(id)
            || (asset.prompt_id.is_none()
                && self.prompt(id).is_some_and(|p| {
                    p.imported
                        && p.text == asset.prompt
                        && p.model == asset.model
                        && p.kind == asset.kind
                }))
    }
    pub fn add_prompt(&mut self, project: Uuid, entry: PromptEntry) -> Result<()> {
        let project = self
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .ok_or_else(|| anyhow::anyhow!("Select a project"))?;
        project.prompts.insert(0, entry);
        Ok(())
    }
    pub fn finish(&mut self, id: Uuid, result: Result<(), String>) {
        if let Some(prompt) = self
            .projects
            .iter_mut()
            .flat_map(|p| &mut p.prompts)
            .find(|p| p.id == id)
        {
            match result {
                Ok(()) => {
                    prompt.state = PromptState::Completed;
                    prompt.detail = None;
                }
                Err(error) => {
                    prompt.state = PromptState::Failed;
                    prompt.detail = Some(error);
                }
            }
        }
    }
}
