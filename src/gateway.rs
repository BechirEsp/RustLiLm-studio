use crate::storage::{save_asset, write_json, Asset, AssetDetails, MediaKind};
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::{
    blocking::{multipart, Client, RequestBuilder, Response},
    Url,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::Read,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const MAX_IMAGE_BYTES: u64 = 100 * 1024 * 1024;
const MAX_VIDEO_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone)]
pub struct Gateway {
    base: Url,
    key: String,
    client: Client,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    pub id: String,
    pub mode: Option<String>,
}

impl Model {
    pub fn supports(&self, kind: MediaKind) -> bool {
        match self.mode.as_deref() {
            Some("image_generation") => kind == MediaKind::Image,
            Some("video_generation") => kind == MediaKind::Video,
            // Plain /models often lacks capabilities; keep unknown aliases usable.
            None => true,
            Some(_) => false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Generation {
    pub model: String,
    pub prompt: String,
    pub project: String,
    pub kind: MediaKind,
    pub size: String,
    pub seconds: u32,
    pub destination: PathBuf,
    #[serde(default)]
    pub logs: PathBuf,
    #[serde(default)]
    pub prompt_id: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingVideo {
    pub id: String,
    pub request: Generation,
    #[serde(default)]
    pub checkpoint: PathBuf,
}

impl Gateway {
    pub fn new(base: &str, key: &str) -> Result<Self> {
        let mut base = Url::parse(base.trim())
            .context("Enter a complete gateway URL, e.g. http://localhost:4000")?;
        if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
            bail!("Gateway URL must use HTTP or HTTPS");
        }
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            bail!("Put your key in the API key field; the URL must not contain credentials, a query, or a fragment");
        }
        let path = base.path().trim_end_matches('/').to_string();
        base.set_path(&format!("{}/", path.strip_suffix("/v1").unwrap_or(&path)));
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(300))
            // A gateway redirect must never forward a credential to another host.
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            base,
            key: key.trim().into(),
            client,
        })
    }

    pub fn endpoint(&self, path: &str) -> Result<Url> {
        Ok(self.base.join(path)?)
    }

    fn auth(&self, request: RequestBuilder) -> RequestBuilder {
        if self.key.is_empty() {
            request
        } else {
            request.bearer_auth(&self.key)
        }
    }

    fn checked(&self, response: Response) -> Result<Response> {
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let mut text = String::new();
        response.take(8192).read_to_string(&mut text).ok();
        if !self.key.is_empty() {
            text = text.replace(&self.key, "[redacted]");
        }
        bail!("Gateway returned {status}: {text}");
    }

    pub fn models(&self) -> Result<Vec<Model>> {
        let response: Value = self
            .checked(
                self.auth(self.client.get(self.endpoint("v1/models")?))
                    .send()?,
            )?
            .json()?;
        let data = response["data"]
            .as_array()
            .context("Gateway /v1/models response has no data array")?;
        let mut models: Vec<Model> = data
            .iter()
            .filter_map(|m| {
                Some(Model {
                    id: m["id"].as_str()?.into(),
                    mode: m
                        .pointer("/model_info/mode")
                        .or_else(|| m.get("mode"))
                        .and_then(Value::as_str)
                        .map(String::from),
                })
            })
            .collect();
        // Optional capability enrichment. Virtual keys may not have access to this endpoint.
        if let Ok(response) = self
            .auth(
                self.client
                    .get(self.endpoint("model/info")?)
                    .timeout(Duration::from_secs(10)),
            )
            .send()
        {
            if response.status().is_success() {
                if let Ok(info) = response.json::<Value>() {
                    if let Some(entries) = info["data"].as_array() {
                        for model in &mut models {
                            if let Some(entry) = entries
                                .iter()
                                .find(|m| m["model_name"].as_str() == Some(&model.id))
                            {
                                model.mode = entry
                                    .pointer("/model_info/mode")
                                    .and_then(Value::as_str)
                                    .map(String::from)
                                    .or(model.mode.take());
                            }
                        }
                    }
                }
            }
        }
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models.dedup_by(|a, b| a.id == b.id);
        if models.is_empty() {
            bail!("No models are available to this API key");
        }
        Ok(models)
    }

    pub fn images(&self, request: &Generation) -> Result<Vec<Asset>> {
        let response: Value = self.checked(self.auth(self.client.post(self.endpoint("v1/images/generations")?))
            .json(&json!({ "model": request.model, "prompt": request.prompt, "n": 1, "size": request.size })).send()?)?.json()?;
        let data = response["data"]
            .as_array()
            .context("Image response has no data array")?;
        if data.is_empty() {
            bail!("Gateway returned no images");
        }
        let mut assets = Vec::new();
        for item in data {
            let bytes = if let Some(encoded) = item["b64_json"].as_str() {
                if encoded.len() as u64 > MAX_IMAGE_BYTES * 4 / 3 + 4 {
                    bail!("Image exceeds the 100 MiB limit");
                }
                STANDARD
                    .decode(encoded)
                    .context("Invalid base64 image from gateway")?
            } else if let Some(url) = item["url"].as_str() {
                self.download(url, MAX_IMAGE_BYTES)?
            } else {
                bail!("Image result has neither b64_json nor url");
            };
            let format =
                image::guess_format(&bytes).context("Gateway did not return a supported image")?;
            let extension = match format {
                image::ImageFormat::Png => "png",
                image::ImageFormat::Jpeg => "jpg",
                image::ImageFormat::WebP => "webp",
                image::ImageFormat::Gif => "gif",
                _ => bail!("Unsupported image format: {format:?}"),
            };
            assets.push(save_asset(
                &request.destination,
                &request.logs,
                &bytes,
                extension,
                AssetDetails {
                    kind: MediaKind::Image,
                    model: &request.model,
                    prompt: &request.prompt,
                    project: &request.project,
                    prompt_id: request.prompt_id,
                },
            )?);
        }
        Ok(assets)
    }

    fn download(&self, url: &str, max: u64) -> Result<Vec<u8>> {
        let url = Url::parse(url)?;
        if !matches!(url.scheme(), "https" | "http") {
            bail!("Media download must use HTTP or HTTPS");
        }
        // Auth is attached only for the exact gateway origin. Signed provider/CDN URLs get no key.
        let request = self.client.get(url.clone());
        let request = if url.origin() == self.base.origin() {
            self.auth(request)
        } else {
            request
        };
        read_limited(self.checked(request.send()?)?, max)
    }

    pub fn submit_video(&self, request: &Generation) -> Result<PendingVideo> {
        let form = multipart::Form::new()
            .text("model", request.model.clone())
            .text("prompt", request.prompt.clone())
            .text("size", request.size.clone())
            .text("seconds", request.seconds.to_string());
        let response: Value = self
            .checked(
                self.auth(self.client.post(self.endpoint("v1/videos")?))
                    .multipart(form)
                    .send()?,
            )?
            .json()?;
        let pending = PendingVideo {
            id: response["id"]
                .as_str()
                .context("Video response has no job ID")?
                .into(),
            request: request.clone(),
            checkpoint: request.logs.join("jobs").join(format!(
                "{}.json",
                request.prompt_id.unwrap_or_else(Uuid::new_v4)
            )),
        };
        // Keep the job ID before polling. A timeout or restart must not submit (and bill) it again.
        write_json(&pending.checkpoint, &pending)?;
        Ok(pending)
    }

    pub fn poll_video(
        &self,
        pending: &PendingVideo,
        cancel: Arc<AtomicBool>,
        mut progress: impl FnMut(String),
    ) -> Result<Asset> {
        let start = Instant::now();
        // Encode opaque LiteLLM IDs as one path segment; never interpolate them into a URL.
        let mut status_url = self.endpoint("v1/videos/")?;
        status_url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Invalid gateway URL"))?
            .pop_if_empty()
            .push(&pending.id);
        loop {
            if cancel.load(Ordering::Relaxed) {
                bail!("Stopped waiting. The gateway job may still run. Resume the saved job instead of submitting again.");
            }
            let response: Value = self
                .checked(self.auth(self.client.get(status_url.clone())).send()?)?
                .json()?;
            let status = response["status"]
                .as_str()
                .context("Video status response has no status")?;
            progress(format!(
                "Video {status} · {}%",
                response["progress"].as_u64().unwrap_or(0)
            ));
            match status {
                "completed" => {
                    let mut content_url = status_url.clone();
                    content_url.path_segments_mut().unwrap().push("content");
                    let bytes = read_limited(
                        self.checked(self.auth(self.client.get(content_url)).send()?)?,
                        MAX_VIDEO_BYTES,
                    )?;
                    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
                        bail!("Gateway video content is not an MP4 file");
                    }
                    let r = &pending.request;
                    let asset = save_asset(
                        &r.destination,
                        &r.logs,
                        &bytes,
                        "mp4",
                        AssetDetails {
                            kind: MediaKind::Video,
                            model: &r.model,
                            prompt: &r.prompt,
                            project: &r.project,
                            prompt_id: r.prompt_id,
                        },
                    )?;
                    let _ = std::fs::remove_file(&pending.checkpoint);
                    return Ok(asset);
                }
                "failed" | "cancelled" | "canceled" => {
                    let error = response.get("error").unwrap_or(&Value::Null).to_string();
                    let error = if self.key.is_empty() {
                        error
                    } else {
                        error.replace(&self.key, "[redacted]")
                    };
                    bail!("Video {status}: {error}");
                }
                "queued" | "in_progress" | "processing" | "pending" => {}
                _ => bail!("Unknown video status '{status}'; the job is saved for resuming"),
            }
            if start.elapsed() > Duration::from_secs(30 * 60) {
                bail!("Video polling timed out after 30 minutes; resume the saved job later");
            }
            for _ in 0..20 {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                thread::sleep(Duration::from_millis(250));
            }
        }
    }
}

fn read_limited(response: Response, max: u64) -> Result<Vec<u8>> {
    if response.content_length().is_some_and(|n| n > max) {
        bail!("Media file exceeds download limit");
    }
    let mut bytes = Vec::new();
    response.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        bail!("Media file exceeds download limit");
    }
    Ok(bytes)
}
