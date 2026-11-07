//! Ollama's experimental image API as implemented in v0.32.5 and its official SDK.
//! Current upstream versions disable generation; discovery/pull still work.
use crate::{
    gateway::{save_image_bytes, Gateway, Generation, Model},
    storage::{Asset, MediaKind, Provider},
};
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

#[derive(Deserialize)]
struct Catalog {
    models: Vec<CatalogModel>,
}
#[derive(Deserialize)]
struct CatalogModel {
    id: String,
    source: String,
}

pub fn catalog() -> Vec<Model> {
    let catalog: Catalog = serde_json::from_str(include_str!("../resources/ollama-models.json"))
        .expect("Bundled Ollama catalog must be valid");
    catalog
        .models
        .into_iter()
        .map(|m| Model {
            id: m.id,
            mode: Some("image_generation".into()),
            downloaded: false,
            supports_edit: Some(false),
            source: Some(m.source),
        })
        .collect()
}

pub fn canonical_model(name: &str) -> String {
    // Registry ports belong to the prefix, while tags belong to the final path component.
    if name.rsplit('/').next().unwrap_or(name).contains(':') {
        name.into()
    } else {
        format!("{name}:latest")
    }
}

impl Gateway {
    pub(crate) fn ollama_models(&self) -> Result<Vec<Model>> {
        let data: Value = self
            .checked(
                self.auth(self.client.get(self.endpoint("api/tags")?))
                    .send()?,
            )?
            .json()?;
        let installed = data["models"]
            .as_array()
            .context("Ollama /api/tags has no models array")?;
        let mut models = catalog();
        for model in installed {
            let Some(id) = model["name"].as_str().or_else(|| model["model"].as_str()) else {
                continue;
            };
            let id = canonical_model(id);
            if let Some(entry) = models.iter_mut().find(|m| canonical_model(&m.id) == id) {
                entry.downloaded = true;
            } else if model["capabilities"]
                .as_array()
                .is_some_and(|caps| caps.iter().any(|c| c == "image" || c == "image_generation"))
            {
                models.push(Model {
                    id,
                    mode: Some("image_generation".into()),
                    downloaded: true,
                    supports_edit: Some(false),
                    source: None,
                });
            }
        }
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    pub fn pull_ollama(
        &self,
        model: &str,
        cancel: Arc<AtomicBool>,
        mut progress: impl FnMut(String),
    ) -> Result<()> {
        if self.provider() != Provider::Ollama {
            bail!("Model downloads are available only for Ollama");
        }
        if cancel.load(Ordering::Relaxed) {
            bail!("Stopped waiting for the model download");
        }
        let data: Value = self
            .checked(
                self.auth(self.client.get(self.endpoint("api/tags")?))
                    .send()?,
            )?
            .json()?;
        let installed = data["models"]
            .as_array()
            .context("Ollama /api/tags has no models array")?;
        if installed.iter().any(|m| {
            m["name"]
                .as_str()
                .or_else(|| m["model"].as_str())
                .is_some_and(|id| canonical_model(id) == canonical_model(model))
        }) {
            return Ok(());
        }
        let response = self.checked(
            self.auth(self.client.post(self.endpoint("api/pull")?))
                .timeout(Duration::from_secs(2 * 60 * 60))
                .json(&json!({"model":model,"stream":true}))
                .send()?,
        )?;
        let mut completed = false;
        read_events(response, 64 * 1024, &cancel, |value| {
            if let Some(error) = value["error"].as_str() {
                bail!("Ollama download failed: {}", self.redacted(error));
            }
            let status = value["status"].as_str().unwrap_or("Downloading model");
            let suffix = match (value["completed"].as_u64(), value["total"].as_u64()) {
                (Some(done), Some(total)) if total > 0 => format!(
                    " · {:.1}% · {:.0}/{:.0} MiB",
                    100. * done as f64 / total as f64,
                    done as f64 / 1048576.,
                    total as f64 / 1048576.
                ),
                _ => String::new(),
            };
            progress(format!("{model}: {status}{suffix}"));
            completed |= status == "success";
            Ok(())
        })?;
        if !completed {
            bail!("Ollama download ended before reporting success. Select the model again to retry/resume.");
        }
        Ok(())
    }

    pub fn ollama_images(
        &self,
        request: &Generation,
        cancel: Arc<AtomicBool>,
        mut progress: impl FnMut(String),
    ) -> Result<Vec<Asset>> {
        if self.provider() != Provider::Ollama {
            bail!("Connect to Ollama first");
        }
        if request.kind != MediaKind::Image {
            bail!("Ollama has no supported video-generation API. Use LiteLLM for video.");
        }
        if request.input_image.is_some() {
            bail!("Ollama's supported experimental API does not expose image editing. Use LiteLLM to edit images.");
        }
        let (width, height) = request
            .size
            .split_once('x')
            .context("Choose an image size such as 1024x1024")?;
        let (width, height): (u32, u32) = (width.parse()?, height.parse()?);
        if width == 0 || height == 0 {
            bail!("Image width and height must be positive");
        }
        self.pull_ollama(&request.model, cancel.clone(), &mut progress)?;
        let response = self.checked(self.auth(self.client.post(self.endpoint("api/generate")?))
            .timeout(Duration::from_secs(30 * 60)).json(&json!({"model":request.model,"prompt":request.prompt,
                "stream":true,"width":width,"height":height})).send()?).context(
                    "Ollama image generation requires a compatible experimental server. Upstream v0.32.6 removed it; v0.32.5 on macOS is the documented supported release")?;
        let mut image = None;
        let mut done = false;
        read_events(response, 140 * 1024 * 1024, &cancel, |value| {
            if let Some(error) = value["error"].as_str() {
                bail!("Ollama generation failed: {}. Current upstream releases may disable experimental image generation.", self.redacted(error));
            }
            if let (Some(step), Some(total)) =
                (value["completed"].as_u64(), value["total"].as_u64())
            {
                progress(format!("Generating image · step {step}/{total}"));
            }
            if let Some(encoded) = value["image"].as_str() {
                if encoded.len() > 140 * 1024 * 1024 {
                    bail!("Ollama image exceeds 100 MiB");
                }
                let bytes = STANDARD
                    .decode(encoded)
                    .context("Invalid image data from Ollama")?;
                if bytes.len() > 100 * 1024 * 1024 {
                    bail!("Ollama image exceeds 100 MiB");
                }
                image = Some(bytes);
            }
            done |= value["done"].as_bool() == Some(true);
            Ok(())
        })?;
        if !done {
            bail!("Ollama stream ended before generation completed");
        }
        let bytes = image.context("Ollama returned no generated image. Choose an image-generation model, not a vision/chat model; current upstream releases may disable this API")?;
        Ok(vec![save_image_bytes(&bytes, request)?])
    }
}

fn read_events(
    reader: impl Read,
    max_line: u64,
    cancel: &AtomicBool,
    mut event: impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("Stopped waiting. The Ollama server may continue processing this request.");
        }
        line.clear();
        let count = reader
            .by_ref()
            .take(max_line + 1)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            return Ok(());
        }
        if count as u64 > max_line {
            bail!("Ollama response event exceeds the size limit");
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        event(serde_json::from_slice(&line).context("Ollama returned invalid streaming JSON")?)?;
    }
}
