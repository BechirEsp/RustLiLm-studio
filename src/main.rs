#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use gpui::{prelude::*, *};
use gpui_component::{
    button::*,
    checkbox::Checkbox,
    input::{Input, InputState},
    select::{Select, SelectEvent, SelectItem, SelectState},
    Disableable, IndexPath, Root, Theme, ThemeMode,
};
use rustlilm_studio::{
    gateway::{Gateway, Generation, Model, PendingVideo},
    projects::{ProjectBook, PromptEntry, PromptState},
    storage::{self, Asset, MediaKind, Provider, Settings},
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

enum Event {
    Models(u64, Gateway, Result<Vec<Model>, String>),
    Folder(Option<PathBuf>),
    Progress(String),
    Finished(Option<Uuid>, Result<Vec<Asset>, String>),
    Clipboard(Result<Vec<u8>, String>),
    Reference(Option<Result<PathBuf, String>>),
    Pull(u64, String, Result<(), String>),
    PullProgress(u64, String),
    Credential(Provider, String, Result<Option<String>, String>),
    CredentialSaved(Provider, String, Result<(), String>),
}
#[derive(Clone, Copy, PartialEq)]
enum Page {
    Gateway,
    Output,
    Studio,
}
struct Toast {
    message: String,
    expires: Instant,
}

#[derive(Clone)]
struct ModelChoice {
    id: String,
    downloaded: Option<bool>,
}
impl SelectItem for ModelChoice {
    type Value = String;
    fn title(&self) -> SharedString {
        match self.downloaded {
            Some(true) => format!("{} · Installed", self.id).into(),
            Some(false) => format!("{} · Download", self.id).into(),
            None => self.id.clone().into(),
        }
    }
    fn value(&self) -> &String {
        &self.id
    }
}

struct Studio {
    settings: Settings,
    draft_settings: Settings,
    page: Page,
    settings_open: bool,
    pending_settings: Option<Settings>,
    connected_key: String,
    connecting: bool,
    connect_epoch: u64,
    url: Entity<InputState>,
    key: Entity<InputState>,
    folder: Entity<InputState>,
    new_project: Entity<InputState>,
    prompt: Entity<InputState>,
    models: Vec<Model>,
    model: Entity<SelectState<Vec<ModelChoice>>>,
    _model_subscription: Subscription,
    input_image: Option<PathBuf>,
    jobs_open: bool,
    pending_jobs: Vec<PendingVideo>,
    reference_loading: bool,
    pulling: Option<String>,
    pull_epoch: u64,
    pull_cancel: Arc<AtomicBool>,
    size: Entity<SelectState<Vec<String>>>,
    seconds: Entity<SelectState<Vec<String>>>,
    kind: MediaKind,
    gateway: Option<Gateway>,
    book: ProjectBook,
    selected_project: Option<Uuid>,
    selected_prompt: Option<Uuid>,
    expanded: HashSet<Uuid>,
    drafts: HashMap<Uuid, String>,
    gallery: Vec<Asset>,
    viewer: Option<Asset>,
    toast: Option<Toast>,
    focus: FocusHandle,
    busy: bool,
    running_video: bool,
    folder_busy: bool,
    status: String,
    error: bool,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    cancel: Arc<AtomicBool>,
}

fn input(
    value: String,
    placeholder: &str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    cx.new(|cx| {
        let mut state = InputState::new(window, cx).placeholder(placeholder.to_string());
        state.set_value(value, window, cx);
        state
    })
}

impl Studio {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (settings, status, error) = match Settings::load() {
            Ok(s) => (s, "Connect your gateway to get started.".into(), false),
            Err(e) => (
                Settings::default(),
                format!("Could not load saved settings: {e}"),
                true,
            ),
        };
        let connected_key = provider_key(settings.provider);
        let (tx, rx) = mpsc::channel();
        let model = cx.new(|cx| {
            SelectState::new(Vec::<ModelChoice>::new(), None, window, cx).searchable(true)
        });
        let model_subscription = cx.subscribe(
            &model,
            |this, _, event: &SelectEvent<Vec<ModelChoice>>, cx| {
                if let SelectEvent::Confirm(Some(model)) = event {
                    this.download_model(model.clone(), cx);
                }
            },
        );
        let this = Self {
            url: input(
                settings.endpoint_url().to_string(),
                "http://localhost:4000 or http://localhost:11434",
                window,
                cx,
            ),
            key: cx.new(|cx| {
                let mut state = InputState::new(window, cx)
                    .placeholder("API key (optional)")
                    .masked(true);
                state.set_value(connected_key.clone(), window, cx);
                state
            }),
            folder: input(
                settings.output.to_string_lossy().into_owned(),
                "Output directory",
                window,
                cx,
            ),
            new_project: input(String::new(), "New project name", window, cx),
            prompt: cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line()
                    .rows(7)
                    .placeholder("Describe the scene, composition, lighting, and mood…")
            }),
            model,
            _model_subscription: model_subscription,
            input_image: None,
            jobs_open: false,
            pending_jobs: Vec::new(),
            reference_loading: false,
            pulling: None,
            pull_epoch: 0,
            pull_cancel: Arc::new(AtomicBool::new(false)),
            size: cx.new(|cx| SelectState::new(image_sizes(), Some(IndexPath::new(0)), window, cx)),
            seconds: cx.new(|cx| {
                SelectState::new(
                    vec!["4".into(), "8".into(), "12".into()],
                    Some(IndexPath::new(0)),
                    window,
                    cx,
                )
            }),
            draft_settings: settings.clone(),
            settings,
            page: Page::Gateway,
            settings_open: false,
            pending_settings: None,
            connected_key,
            connecting: false,
            connect_epoch: 0,
            models: Vec::new(),
            kind: MediaKind::Image,
            gateway: None,
            book: ProjectBook::default(),
            selected_project: None,
            selected_prompt: None,
            expanded: HashSet::new(),
            drafts: HashMap::new(),
            gallery: Vec::new(),
            viewer: None,
            toast: None,
            focus: cx.focus_handle(),
            busy: false,
            running_video: false,
            folder_busy: false,
            status,
            error,
            tx,
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        cx.spawn_in(window, async move |view, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            if view
                .update_in(cx, |view, window, cx| view.receive(window, cx))
                .is_err()
            {
                break;
            }
        })
        .detach();
        this.load_stored_key();
        this
    }

    fn receive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .toast
            .as_ref()
            .is_some_and(|t| Instant::now() >= t.expires)
        {
            self.toast = None;
            cx.notify();
        }
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Models(epoch, gateway, result) => {
                    if epoch != self.connect_epoch {
                        continue;
                    }
                    self.connecting = false;
                    match result {
                        Ok(models) => {
                            if self.settings_open {
                                let Some(settings) = self.pending_settings.take() else {
                                    continue;
                                };
                                if let Err(e) = self.apply_workspace(settings) {
                                    self.fail(e.to_string());
                                    cx.notify();
                                    continue;
                                }
                                self.settings_open = false;
                                self.show_toast("Settings saved");
                            } else {
                                self.page = Page::Output;
                            }
                            self.gateway = Some(gateway);
                            self.connected_key = self.key.read(cx).value().to_string();
                            self.models = models;
                            self.update_models(window, cx);
                            self.status =
                                format!("Connected · {} models available", self.models.len());
                            self.error = false;
                            self.settings.provider = self.gateway.as_ref().unwrap().provider();
                            self.settings.remember_key = self.draft_settings.remember_key;
                            if self.settings.provider == Provider::Ollama {
                                self.settings.ollama_url = self.url.read(cx).value().trim().into();
                            } else {
                                self.settings.gateway_url = self.url.read(cx).value().trim().into();
                            }
                            if let Err(error) = self.settings.save() {
                                self.fail(format!(
                                    "Cannot remember the connection settings: {error}"
                                ));
                            }
                            self.persist_connection_key();
                        }
                        Err(e) => {
                            self.pending_settings = None;
                            self.fail(e);
                        }
                    }
                }
                Event::Folder(folder) => {
                    self.folder_busy = false;
                    if let Some(folder) = folder {
                        self.folder.update(cx, |input, cx| {
                            input.set_value(folder.to_string_lossy().into_owned(), window, cx)
                        });
                    }
                }
                Event::Progress(message) => self.status = message,
                Event::Finished(prompt_id, result) => {
                    self.busy = false;
                    if let Some(id) = prompt_id {
                        self.book
                            .finish(id, result.as_ref().map(|_| ()).map_err(Clone::clone));
                    }
                    match result {
                        Ok(assets) => {
                            let count = assets.len();
                            for asset in assets {
                                self.gallery.insert(0, asset);
                            }
                            self.book.import_assets(&self.gallery);
                            if self.selected_project.is_none() {
                                self.selected_project = self.book.projects.first().map(|p| p.id);
                                if let Some(id) = self.selected_project {
                                    self.expanded.insert(id);
                                }
                            }
                            if let Err(e) =
                                storage::save_gallery(&self.settings.output, &self.gallery)
                            {
                                self.fail(format!(
                                    "Files saved, but gallery could not be saved: {e}"
                                ));
                            } else {
                                self.status = format!("Saved {count} result(s).");
                                self.error = false;
                            }
                        }
                        Err(e) => self.fail(e),
                    }
                    if let Err(e) = self.book.save(&self.settings.output) {
                        self.fail(format!("Could not save prompt history: {e}"));
                    }
                }
                Event::Credential(provider, url, result) => {
                    if self.draft_settings.provider == provider
                        && self.url.read(cx).value().trim() == url
                        && self.key.read(cx).value().is_empty()
                    {
                        match result {
                            Ok(Some(key)) => self
                                .key
                                .update(cx, |input, cx| input.set_value(key, window, cx)),
                            Ok(None) => {}
                            Err(error) => self.fail(error),
                        }
                    }
                }
                Event::CredentialSaved(provider, url, result) => {
                    if provider != self.settings.provider || url != self.settings.endpoint_url() {
                        continue;
                    }
                    if let Err(error) = result {
                        self.fail(format!(
                            "Connected, but the API key was not remembered: {error}"
                        ));
                    }
                }
                Event::Reference(result) => {
                    self.reference_loading = false;
                    match result {
                        Some(Ok(path)) => {
                            self.input_image = Some(path);
                            self.set_kind(MediaKind::Image, window, cx);
                            self.status =
                                "Reference image ready. Describe the changes and click Edit image."
                                    .into();
                            self.error = false;
                        }
                        Some(Err(error)) => self.fail(error),
                        None => {}
                    }
                }
                Event::PullProgress(epoch, message) => {
                    if epoch == self.pull_epoch {
                        self.status = message;
                    }
                }
                Event::Pull(epoch, model, result) => {
                    if epoch != self.pull_epoch {
                        continue;
                    }
                    self.pulling = None;
                    match result {
                        Ok(()) => {
                            for item in &mut self.models {
                                if rustlilm_studio::ollama::canonical_model(&item.id)
                                    == rustlilm_studio::ollama::canonical_model(&model)
                                {
                                    item.downloaded = true;
                                }
                            }
                            self.update_models(window, cx);
                            self.show_toast("Model downloaded");
                            self.status = "Model is ready.".into();
                            self.error = false;
                        }
                        Err(error) => self.fail(error),
                    }
                }
                Event::Clipboard(result) => match result {
                    Ok(bytes) => {
                        cx.write_to_clipboard(ClipboardItem::new_image(&gpui::Image::from_bytes(
                            gpui::ImageFormat::Png,
                            bytes,
                        )));
                        self.show_toast("Image copied");
                        self.error = false;
                    }
                    Err(e) => self.fail(e),
                },
            }
            cx.notify();
        }
    }

    fn show_toast(&mut self, message: &str) {
        self.toast = Some(Toast {
            message: message.into(),
            expires: Instant::now() + Duration::from_secs(3),
        });
    }
    fn fail(&mut self, message: String) {
        self.status = message;
        self.error = true;
    }

    fn connect(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.connecting {
            return;
        }
        match Gateway::with_provider(
            &self.url.read(cx).value(),
            &self.key.read(cx).value(),
            self.draft_settings.provider,
        ) {
            Ok(gateway) => {
                self.connect_epoch += 1;
                let epoch = self.connect_epoch;
                self.connecting = true;
                self.error = false;
                self.status = "Discovering available models…".into();
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let result = gateway.models().map_err(|e| e.to_string());
                    let _ = tx.send(Event::Models(epoch, gateway, result));
                });
            }
            Err(e) => self.fail(e.to_string()),
        }
        cx.notify();
    }

    fn update_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.model.read(cx).selected_value().cloned();
        let items = self
            .models
            .iter()
            .filter(|m| m.supports_operation(self.kind, self.input_image.is_some()))
            .map(|m| ModelChoice {
                id: m.id.clone(),
                downloaded: if self.active_provider() == Provider::Ollama {
                    Some(m.downloaded)
                } else {
                    None
                },
            })
            .collect::<Vec<_>>();
        let index = previous
            .as_ref()
            .and_then(|id| items.iter().position(|m| &m.id == id))
            .or(
                if items.is_empty() || self.active_provider() == Provider::Ollama {
                    None
                } else {
                    Some(0)
                },
            )
            .map(IndexPath::new);
        self.model.update(cx, |select, cx| {
            select.set_items(items, window, cx);
            select.set_selected_index(index, window, cx);
        });
    }
    fn set_kind(&mut self, kind: MediaKind, window: &mut Window, cx: &mut Context<Self>) {
        self.kind = kind;
        if kind == MediaKind::Video {
            self.input_image = None;
        }
        self.update_models(window, cx);
        self.size.update(cx, |select, cx| {
            select.set_items(
                if kind == MediaKind::Image {
                    image_sizes()
                } else {
                    vec!["1280x720".into(), "720x1280".into()]
                },
                window,
                cx,
            );
            select.set_selected_index(Some(IndexPath::new(0)), window, cx);
        });
        cx.notify();
    }
    fn browse(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.folder_busy {
            return;
        }
        self.folder_busy = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let folder = rfd::FileDialog::new()
                .set_title("Choose generation output folder")
                .pick_folder();
            let _ = tx.send(Event::Folder(folder));
        });
        cx.notify();
    }
    fn candidate_settings(&self, cx: &App) -> anyhow::Result<Settings> {
        let mut settings = self.draft_settings.clone();
        if settings.provider == Provider::Ollama {
            settings.ollama_url = self.url.read(cx).value().trim().into();
        } else {
            settings.gateway_url = self.url.read(cx).value().trim().into();
        }
        let value = self.folder.read(cx).value();
        if value.trim().is_empty() {
            anyhow::bail!("Choose an output folder");
        }
        std::fs::create_dir_all(value.trim())?;
        settings.output = std::fs::canonicalize(value.trim())?;
        Ok(settings)
    }
    fn apply_workspace(&mut self, settings: Settings) -> anyhow::Result<()> {
        if self.page == Page::Studio && settings.output == self.settings.output {
            settings.save()?;
            self.settings = settings;
            return Ok(());
        }
        let gallery = storage::load_gallery(&settings.output)?;
        let book = ProjectBook::load(&settings.output, &gallery)?;
        settings.save()?;
        self.selected_project = book.projects.first().map(|p| p.id);
        self.selected_prompt = None;
        self.expanded.clear();
        self.drafts.clear();
        if let Some(id) = self.selected_project {
            self.expanded.insert(id);
        }
        self.gallery = gallery;
        self.book = book;
        self.settings = settings;
        Ok(())
    }
    fn enter_studio(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        match self
            .candidate_settings(cx)
            .and_then(|s| self.apply_workspace(s))
        {
            Ok(()) => {
                self.page = Page::Studio;
                self.status = "Choose or create a project to begin.".into();
                self.error = false;
            }
            Err(e) => self.fail(e.to_string()),
        }
        cx.notify();
    }
    fn open_settings(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.draft_settings = self.settings.clone();
        self.settings_open = true;
        self.reset_settings_inputs(window, cx);
        cx.notify();
    }
    fn reset_settings_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.url.update(cx, |i, cx| {
            i.set_value(self.settings.endpoint_url().to_string(), window, cx)
        });
        self.folder.update(cx, |i, cx| {
            i.set_value(
                self.settings.output.to_string_lossy().into_owned(),
                window,
                cx,
            )
        });
        self.key.update(cx, |i, cx| {
            i.set_value(self.connected_key.clone(), window, cx)
        });
    }
    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = false;
        self.pending_settings = None;
        self.connect_epoch += 1;
        self.connecting = false;
        self.draft_settings = self.settings.clone();
        self.reset_settings_inputs(window, cx);
        self.status = "Ready to create.".into();
        self.error = false;
        window.focus(&self.focus);
        cx.notify();
    }
    fn save_settings(&mut self, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.connecting {
            return;
        }
        let settings = match self.candidate_settings(cx) {
            Ok(s) => s,
            Err(e) => {
                self.fail(e.to_string());
                cx.notify();
                return;
            }
        };
        if settings.provider == self.settings.provider
            && settings.endpoint_url() == self.settings.endpoint_url()
            && self.key.read(cx).value().as_ref() == self.connected_key
        {
            match self.apply_workspace(settings) {
                Ok(()) => {
                    self.settings_open = false;
                    self.show_toast("Settings saved");
                    self.status = "Ready to create.".into();
                    self.error = false;
                    self.persist_connection_key();
                }
                Err(e) => self.fail(e.to_string()),
            }
        } else {
            self.pending_settings = Some(settings);
            self.connect(event, window, cx);
        }
        cx.notify();
    }

    fn select_project(
        &mut self,
        id: Uuid,
        toggle: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(old) = self.selected_project {
            self.drafts
                .insert(old, self.prompt.read(cx).value().to_string());
        }
        if toggle && self.selected_project == Some(id) && self.expanded.contains(&id) {
            self.expanded.remove(&id);
        } else {
            self.expanded.insert(id);
        }
        self.selected_project = Some(id);
        self.selected_prompt = None;
        self.input_image = None;
        self.update_models(window, cx);
        let text = self.drafts.get(&id).cloned().unwrap_or_default();
        self.prompt
            .update(cx, |i, cx| i.set_value(text, window, cx));
        cx.notify();
    }
    fn create_project(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let mut candidate = self.book.clone();
        let result = candidate
            .create(&self.new_project.read(cx).value())
            .and_then(|id| {
                candidate.save(&self.settings.output)?;
                Ok(id)
            });
        match result {
            Ok(id) => {
                self.book = candidate;
                self.select_project(id, false, window, cx);
                self.new_project
                    .update(cx, |i, cx| i.set_value("", window, cx));
                self.show_toast("Project created");
                self.error = false;
            }
            Err(e) => self.fail(e.to_string()),
        }
        cx.notify();
    }
    fn select_prompt(
        &mut self,
        project_id: Uuid,
        id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.book.prompt(id).cloned() else {
            return;
        };
        self.select_project(project_id, false, window, cx);
        self.selected_prompt = Some(id);
        self.prompt
            .update(cx, |i, cx| i.set_value(entry.text, window, cx));
        self.input_image = entry.input_image.clone();
        self.set_kind(entry.kind, window, cx);
        self.model
            .update(cx, |s, cx| s.set_selected_value(&entry.model, window, cx));
        self.size
            .update(cx, |s, cx| s.set_selected_value(&entry.size, window, cx));
        self.seconds.update(cx, |s, cx| {
            s.set_selected_value(&entry.seconds.to_string(), window, cx)
        });
        if entry.provider != self.active_provider() {
            self.model
                .update(cx, |s, cx| s.set_selected_index(None, window, cx));
            self.fail(format!("This prompt used {}. Connect to it in Settings, or choose a model from the current provider.", entry.provider.label()));
        } else if self.model.read(cx).selected_value().is_none() {
            self.fail(
                "This prompt's model is unavailable. Choose a model before generating.".into(),
            );
        } else {
            self.status = "Prompt restored. You can edit and generate again.".into();
            self.error = false;
        }
        if entry.provider == self.active_provider() {
            if let Some(model) = self.model.read(cx).selected_value().cloned() {
                self.download_model(model, cx);
            }
        }
        cx.notify();
    }
    fn generate(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.pulling.is_some() || self.reference_loading {
            return;
        }
        let Some(gateway) = self.gateway.clone() else {
            self.fail("Connect a gateway first".into());
            return;
        };
        let result = (|| -> anyhow::Result<Generation> {
            let project_id = self
                .selected_project
                .ok_or_else(|| anyhow::anyhow!("Create or select a project on the left"))?;
            let project = self
                .book
                .project(project_id)
                .ok_or_else(|| anyhow::anyhow!("Select a project"))?
                .name
                .clone();
            let prompt = self.prompt.read(cx).value().trim().to_string();
            if prompt.is_empty() {
                anyhow::bail!("Write a prompt first");
            }
            let model = self
                .model
                .read(cx)
                .selected_value()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Choose a model"))?;
            let size = self
                .size
                .read(cx)
                .selected_value()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Choose a size"))?;
            let seconds = self
                .seconds
                .read(cx)
                .selected_value()
                .and_then(|v| v.parse().ok())
                .unwrap_or(4);
            if gateway.provider() == Provider::Ollama
                && (self.kind == MediaKind::Video || self.input_image.is_some())
            {
                anyhow::bail!("Use LiteLLM for image editing or video. Ollama currently supports only experimental image generation on compatible servers.");
            }
            let id = Uuid::new_v4();
            let request = Generation {
                model: model.clone(),
                prompt: prompt.clone(),
                destination: self.settings.destination(&project)?,
                project,
                kind: self.kind,
                size: size.clone(),
                seconds,
                logs: storage::metadata_dir(&self.settings.output),
                prompt_id: Some(id),
                input_image: self.input_image.clone(),
            };
            let mut candidate = self.book.clone();
            candidate.add_prompt(
                project_id,
                PromptEntry {
                    id,
                    text: prompt,
                    model,
                    kind: self.kind,
                    size,
                    seconds,
                    created: chrono::Utc::now(),
                    state: PromptState::Submitted,
                    detail: None,
                    imported: false,
                    provider: gateway.provider(),
                    input_image: self.input_image.clone(),
                },
            )?;
            candidate.save(&self.settings.output)?;
            self.book = candidate;
            self.selected_prompt = Some(id);
            Ok(request)
        })();
        match result {
            Ok(request) => {
                self.start_job(request.kind);
                let tx = self.tx.clone();
                let cancel = self.cancel.clone();
                std::thread::spawn(move || {
                    let result = (|| -> anyhow::Result<Vec<Asset>> {
                        if request.kind == MediaKind::Image {
                            if gateway.provider() == Provider::Ollama {
                                gateway.ollama_images(&request, cancel, |s| {
                                    let _ = tx.send(Event::Progress(s));
                                })
                            } else {
                                gateway.images(&request)
                            }
                        } else {
                            let pending = gateway.submit_video(&request)?;
                            gateway
                                .poll_video(&pending, cancel, |s| {
                                    let _ = tx.send(Event::Progress(s));
                                })
                                .map(|a| vec![a])
                        }
                    })();
                    let _ = tx.send(Event::Finished(
                        request.prompt_id,
                        result.map_err(|e| e.to_string()),
                    ));
                });
            }
            Err(e) => self.fail(e.to_string()),
        }
        cx.notify();
    }
    fn start_job(&mut self, kind: MediaKind) {
        self.running_video = kind == MediaKind::Video;
        self.busy = true;
        self.error = false;
        self.status = "Generating…".into();
        self.cancel = Arc::new(AtomicBool::new(false));
    }
    fn resume(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.pulling.is_some() {
            return;
        }
        if self.active_provider() == Provider::Ollama {
            self.fail("Use the LiteLLM gateway that created the video job.".into());
            cx.notify();
            return;
        }
        match rustlilm_studio::database::Database::workspace(&storage::metadata_dir(
            &self.settings.output,
        ))
        .and_then(|db| db.jobs())
        {
            Ok(jobs) => {
                self.pending_jobs = jobs;
                self.jobs_open = true;
            }
            Err(error) => self.fail(error.to_string()),
        }
        cx.notify();
    }
    fn resume_job(&mut self, pending: PendingVideo, cx: &mut Context<Self>) {
        let Some(gateway) = self.gateway.clone() else {
            return;
        };
        self.jobs_open = false;
        self.start_job(MediaKind::Video);
        let tx = self.tx.clone();
        let cancel = self.cancel.clone();
        std::thread::spawn(move || {
            let result = gateway
                .poll_video(&pending, cancel, |s| {
                    let _ = tx.send(Event::Progress(s));
                })
                .map(|a| vec![a]);
            let _ = tx.send(Event::Finished(
                pending.request.prompt_id,
                result.map_err(|e| e.to_string()),
            ));
        });
        cx.notify();
    }

    fn copy(&mut self, path: PathBuf) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<Vec<u8>> {
                let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
                let mut limits = image::Limits::default();
                limits.max_alloc = Some(512 * 1024 * 1024);
                reader.limits(limits);
                let image = reader.decode()?;
                let mut bytes = std::io::Cursor::new(Vec::new());
                image.write_to(&mut bytes, image::ImageFormat::Png)?;
                Ok(bytes.into_inner())
            })();
            let _ = tx.send(Event::Clipboard(result.map_err(|e| e.to_string())));
        });
    }

    fn active_provider(&self) -> Provider {
        self.gateway
            .as_ref()
            .map(Gateway::provider)
            .unwrap_or(self.settings.provider)
    }
    fn load_stored_key(&self) {
        if !self.draft_settings.remember_key {
            return;
        }
        let provider = self.draft_settings.provider;
        let url = self.draft_settings.endpoint_url().to_string();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result =
                rustlilm_studio::credentials::load(provider, &url).map_err(|e| e.to_string());
            let _ = tx.send(Event::Credential(provider, url, result));
        });
    }
    fn persist_connection_key(&self) {
        let provider = self.settings.provider;
        let url = self.settings.endpoint_url().to_string();
        let key = if self.settings.remember_key {
            Some(self.connected_key.clone())
        } else {
            None
        };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = rustlilm_studio::credentials::save(provider, &url, key.as_deref())
                .map_err(|e| e.to_string());
            let _ = tx.send(Event::CredentialSaved(provider, url, result));
        });
    }
    fn choose_provider(&mut self, provider: Provider, window: &mut Window, cx: &mut Context<Self>) {
        if self.connecting {
            return;
        }
        if self.draft_settings.provider == Provider::Ollama {
            self.draft_settings.ollama_url = self.url.read(cx).value().to_string();
        } else {
            self.draft_settings.gateway_url = self.url.read(cx).value().to_string();
        }
        self.draft_settings.provider = provider;
        self.url.update(cx, |input, cx| {
            input.set_value(self.draft_settings.endpoint_url().to_string(), window, cx)
        });
        let key = if provider == self.settings.provider {
            self.connected_key.clone()
        } else {
            provider_key(provider)
        };
        self.key
            .update(cx, |input, cx| input.set_value(key, window, cx));
        self.load_stored_key();
        cx.notify();
    }
    fn download_model(&mut self, model: String, cx: &mut Context<Self>) {
        if self.busy || self.pulling.is_some() || self.active_provider() != Provider::Ollama {
            return;
        }
        if !self.models.iter().any(|m| m.id == model && !m.downloaded) {
            return;
        }
        let Some(gateway) = self.gateway.clone() else {
            return;
        };
        self.pull_epoch += 1;
        let epoch = self.pull_epoch;
        self.pulling = Some(model.clone());
        self.pull_cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.pull_cancel.clone();
        self.status = format!("Downloading {model} to the connected Ollama server…");
        self.error = false;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = gateway.pull_ollama(&model, cancel, |s| {
                let _ = tx.send(Event::PullProgress(epoch, s));
            });
            let _ = tx.send(Event::Pull(epoch, model, result.map_err(|e| e.to_string())));
        });
        cx.notify();
    }
    fn upload_reference(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.reference_loading {
            return;
        }
        self.reference_loading = true;
        let logs = storage::metadata_dir(&self.settings.output);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = rfd::FileDialog::new()
                .set_title("Choose an image to edit")
                .add_filter("Images", &["png", "jpg", "jpeg", "webp", "gif"])
                .pick_file()
                .map(|path| storage::prepare_reference(&path, &logs).map_err(|e| e.to_string()));
            let _ = tx.send(Event::Reference(result));
        });
        cx.notify();
    }
    fn begin_edit(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.reference_loading || self.pulling.is_some() {
            return;
        }
        self.viewer = None;
        self.selected_prompt = None;
        self.input_image = None;
        self.reference_loading = true;
        self.prompt.update(cx, |i, cx| i.set_value("", window, cx));
        self.set_kind(MediaKind::Image, window, cx);
        let logs = storage::metadata_dir(&self.settings.output);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = storage::prepare_reference(&path, &logs).map_err(|e| e.to_string());
            let _ = tx.send(Event::Reference(Some(result)));
        });
        cx.notify();
    }
    fn clear_reference(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input_image = None;
        self.update_models(window, cx);
        cx.notify();
    }
    fn connection_fields(&self, cx: &mut Context<Self>) -> AnyElement {
        div().flex().flex_col().gap_3().child(label("Provider"))
            .child(div().flex().gap_2()
                .child(Button::new("provider-litellm").label("LiteLLM").when(self.draft_settings.provider == Provider::LiteLlm, |b| b.primary()).disabled(self.connecting)
                    .on_click(cx.listener(|this, _, w, cx| this.choose_provider(Provider::LiteLlm, w, cx))))
                .child(Button::new("provider-ollama").label("Ollama").when(self.draft_settings.provider == Provider::Ollama, |b| b.primary()).disabled(self.connecting)
                    .on_click(cx.listener(|this, _, w, cx| this.choose_provider(Provider::Ollama, w, cx)))))
            .child(label("Server URL")).child(Input::new(&self.url).disabled(self.connecting))
            .child(label("API key (optional for local Ollama)")).child(Input::new(&self.key).disabled(self.connecting))
            .child(Checkbox::new("remember-key").label("Remember API key in the OS credential store").checked(self.draft_settings.remember_key).disabled(self.connecting)
                .on_click(cx.listener(|this, value, _, cx| { this.draft_settings.remember_key = *value; cx.notify(); })))
            .when(self.draft_settings.provider == Provider::Ollama, |col| col.child(div().text_xs().text_color(rgb(0xf0cf91))
                .child("Ollama's image API is experimental (documented on macOS v0.32.5; removed upstream in v0.32.6). Editing/video require LiteLLM. Selecting a missing model downloads it to this server; files can be several GB.")))
            .into_any_element()
    }
    fn jobs_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .absolute()
            .inset_0()
            .bg(rgba(0x080b14ee))
            .p_8()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .id("video-jobs")
                    .w(px(650.))
                    .max_h_full()
                    .overflow_y_scroll()
                    .p_5()
                    .rounded_xl()
                    .bg(rgb(0x1b2130))
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(label("Saved video jobs"))
                            .child(Button::new("close-jobs").label("Close ×").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.jobs_open = false;
                                    cx.notify();
                                }),
                            )),
                    )
                    .when(self.pending_jobs.is_empty(), |col| {
                        col.child(label("No unfinished video jobs in this workspace."))
                    })
                    .children(
                        self.pending_jobs
                            .iter()
                            .cloned()
                            .enumerate()
                            .map(|(index, job)| {
                                div()
                                    .id(("job", index))
                                    .p_3()
                                    .border_1()
                                    .border_color(rgb(0x30384a))
                                    .rounded_lg()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(label(&format!(
                                        "{} · {}",
                                        job.request.project, job.request.model
                                    )))
                                    .child(shorten(&job.request.prompt, 180))
                                    .child(
                                        Button::new("resume-job")
                                            .primary()
                                            .label("Resume")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.resume_job(job.clone(), cx)
                                            })),
                                    )
                            }),
                    ),
            )
            .into_any_element()
    }
    fn sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let projects = self.book.projects.clone();
        div()
            .w(px(270.))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x151923))
            .border_r_1()
            .border_color(rgb(0x292e3d))
            .child(
                div()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(div().font_weight(FontWeight::BOLD).child("Projects"))
                            .child(label(&format!("{}", projects.len()))),
                    )
                    .child(Input::new(&self.new_project).disabled(self.busy))
                    .child(
                        Button::new("new-project")
                            .primary()
                            .label("+ Create project")
                            .disabled(self.busy)
                            .on_click(cx.listener(Self::create_project)),
                    ),
            )
            .child(
                div()
                    .id("project-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_3()
                    .pb_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(projects.is_empty(), |col| {
                        col.child(div().p_3().text_sm().text_color(rgb(0x99a5bc)).child(
                            "Create your first project above. Your prompts will appear here.",
                        ))
                    })
                    .children(projects.into_iter().map(|project| {
                        let id = project.id;
                        let expanded = self.expanded.contains(&id);
                        let selected = self.selected_project == Some(id);
                        div()
                            .id(SharedString::from(format!("project-{id}")))
                            .rounded_lg()
                            .overflow_hidden()
                            .child(
                                div()
                                    .id("project-heading")
                                    .px_3()
                                    .py_3()
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .bg(if selected {
                                        rgb(0x293747)
                                    } else {
                                        rgb(0x1d2330)
                                    })
                                    .hover(|s| s.bg(rgb(0x303d50)))
                                    .child(if expanded { "▾" } else { "▸" })
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_sm()
                                            .child(project.name.clone()),
                                    )
                                    .child(label(&project.prompts.len().to_string()))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.select_project(id, true, window, cx)
                                    })),
                            )
                            .when(expanded, |col| {
                                col.child(
                                    div()
                                        .pl_3()
                                        .py_2()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .when(project.prompts.is_empty(), |col| {
                                            col.child(
                                                div()
                                                    .p_2()
                                                    .text_xs()
                                                    .text_color(rgb(0x99a5bc))
                                                    .child("No prompts yet"),
                                            )
                                        })
                                        .children(project.prompts.into_iter().map(|entry| {
                                            let prompt_id = entry.id;
                                            let state = match entry.state {
                                                PromptState::Submitted => "Submitted",
                                                PromptState::Completed => "Completed",
                                                PromptState::Failed => "Needs attention",
                                            };
                                            div()
                                                .id(SharedString::from(format!(
                                                    "prompt-{prompt_id}"
                                                )))
                                                .p_2()
                                                .rounded_lg()
                                                .cursor_pointer()
                                                .bg(if self.selected_prompt == Some(prompt_id) {
                                                    rgb(0x2c3d4d)
                                                } else {
                                                    rgb(0x151923)
                                                })
                                                .hover(|s| s.bg(rgb(0x283345)))
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .overflow_hidden()
                                                        .child(shorten(&entry.text, 72)),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(rgb(0x99a5bc))
                                                        .child(format!(
                                                            "{} · {state}",
                                                            entry
                                                                .created
                                                                .with_timezone(&chrono::Local)
                                                                .format("%b %d · %H:%M")
                                                        )),
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.select_prompt(
                                                            id, prompt_id, window, cx,
                                                        )
                                                    },
                                                ))
                                        })),
                                )
                            })
                            .into_any_element()
                    })),
            )
            .child(
                div().p_4().border_t_1().border_color(rgb(0x292e3d)).child(
                    Button::new("settings")
                        .label("Settings")
                        .disabled(self.busy || self.pulling.is_some() || self.reference_loading)
                        .on_click(cx.listener(Self::open_settings)),
                ),
            )
            .into_any_element()
    }
    fn composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let locked = self.busy || self.pulling.is_some() || self.reference_loading;
        let ollama = self.active_provider() == Provider::Ollama;
        let editing = self.input_image.is_some();
        let source = self
            .model
            .read(cx)
            .selected_value()
            .and_then(|id| self.models.iter().find(|m| &m.id == id))
            .and_then(|m| m.source.clone());
        div().flex_shrink_0().p_5().border_t_1().border_color(rgb(0x292e3d)).bg(rgb(0x171c27)).flex().gap_5()
            .child(div().flex_1().min_w_0().flex().flex_col().gap_2()
                .child(div().flex().justify_between().items_center().gap_2()
                    .child(label(if editing { "EDIT PROMPT" } else { "PROMPT" }))
                    .child(Button::new("upload-image").label("Upload image…").disabled(locked || ollama).on_click(cx.listener(Self::upload_reference)))
                    .child(Button::new("new-prompt").label("New prompt").disabled(locked).on_click(cx.listener(|this, _, w, cx| {
                        this.selected_prompt = None; this.clear_reference(w,cx); this.prompt.update(cx, |i,cx| i.set_value("",w,cx)); cx.notify();
                    }))))
                .when_some(self.input_image.as_ref(), |col, path| col.child(div().flex().items_center().gap_3()
                    .child(img(path.clone()).w(px(72.)).h(px(64.)).object_fit(ObjectFit::Contain))
                    .child(label("Reference image · describe the changes below"))
                    .child(Button::new("clear-reference").label("Remove ×").disabled(locked).on_click(cx.listener(|this, _, w, cx| this.clear_reference(w,cx))))))
                .child(Input::new(&self.prompt).h(px(if editing { 170. } else { 235. })).disabled(locked || self.selected_project.is_none())))
            .child(div().w(px(285.)).flex_shrink_0().flex().flex_col().gap_2()
                .child(label(&format!("OUTPUT · {}", self.active_provider().label())))
                .child(div().flex().gap_2()
                    .child(Button::new("images").label("Image").when(self.kind == MediaKind::Image, |b| b.primary()).disabled(locked)
                        .on_click(cx.listener(|this, _, w, cx| this.set_kind(MediaKind::Image,w,cx))))
                    .child(Button::new("videos").label("Video").when(self.kind == MediaKind::Video, |b| b.primary()).disabled(locked || ollama)
                        .on_click(cx.listener(|this, _, w, cx| this.set_kind(MediaKind::Video,w,cx)))))
                .child(label("Model")).child(Select::new(&self.model).disabled(locked))
                .when(ollama, |col| col.child(div().text_xs().text_color(rgb(0xf0cf91)).child("Missing models download automatically. Experimental image generation requires a compatible Ollama server; editing/video use LiteLLM.")))
                .when_some(source, |col, source| col.child(Button::new("model-source").label("Model details & license").on_click(move |_, _, cx| cx.open_url(&source))))
                .child(div().flex().gap_2()
                    .child(div().flex_1().child(label("Size")).child(Select::new(&self.size).disabled(locked)))
                    .when(self.kind == MediaKind::Video, |row| row.child(div().w(px(80.)).child(label("Seconds")).child(Select::new(&self.seconds).disabled(locked)))))
                .child(Button::new("generate").primary().label(if editing { "Edit image" } else if self.kind == MediaKind::Image { "Generate image" } else { "Generate video" })
                    .loading(self.busy).disabled(locked || self.selected_project.is_none() || (ollama && editing)).on_click(cx.listener(Self::generate)))
                .when(self.pulling.is_some(), |col| col.child(Button::new("stop-download").label("Stop waiting for download").on_click(cx.listener(|this, _, _, cx| {
                        this.pull_cancel.store(true,Ordering::Relaxed); this.pull_epoch += 1; this.pulling = None;
                        this.status = "Stopped waiting for download. Ollama may continue; selecting the model again checks/resumes it.".into(); cx.notify();
                    }))))
                .when(self.kind == MediaKind::Video && !ollama, |col| col.child(Button::new("resume").label("Resume saved video…").disabled(locked).on_click(cx.listener(Self::resume))))
                .when(self.busy && (self.running_video || ollama), |col| col.child(Button::new("stop").label("Stop waiting").on_click(cx.listener(|this, _, _, _| this.cancel.store(true,Ordering::Relaxed))))))
            .into_any_element()
    }
    fn card(&self, asset: &Asset, cx: &mut Context<Self>) -> AnyElement {
        let reveal = asset.path.clone();
        let copy = asset.path.clone();
        let preview = asset.clone();
        let video = asset.path.clone();
        let edit = asset.path.clone();
        div()
            .id(SharedString::from(asset.id.to_string()))
            .group("asset")
            .relative()
            .w(px(284.))
            .rounded_xl()
            .overflow_hidden()
            .bg(rgb(0x1b2130))
            .border_1()
            .border_color(rgb(0x30384a))
            .child(
                div()
                    .id("preview")
                    .cursor_pointer()
                    .child(if asset.kind == MediaKind::Image {
                        img(asset.path.clone())
                            .w_full()
                            .h(px(210.))
                            .object_fit(ObjectFit::Contain)
                            .into_any_element()
                    } else {
                        div()
                            .h(px(210.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_3xl()
                            .child("▶ MP4")
                            .into_any_element()
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if preview.kind == MediaKind::Image {
                            this.viewer = Some(preview.clone());
                            window.focus(&this.focus);
                            cx.notify();
                        } else if let Err(e) = open_file(&video) {
                            this.fail(e.to_string());
                            cx.notify();
                        }
                    })),
            )
            .child(
                div()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().child(shorten(&asset.prompt, 100)))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x99a5bc))
                            .child(asset.model.clone()),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top_2()
                    .right_2()
                    .flex()
                    .gap_1()
                    .opacity(0.)
                    .group_hover("asset", |s| s.opacity(1.))
                    .when(asset.kind == MediaKind::Image, |row| {
                        row.child(
                            Button::new("edit")
                                .label("Edit")
                                .disabled(self.busy || self.active_provider() == Provider::Ollama)
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    this.begin_edit(edit.clone(), w, cx)
                                })),
                        )
                        .child(
                            Button::new("copy").label("Copy image").on_click(
                                cx.listener(move |this, _, _, _| this.copy(copy.clone())),
                            ),
                        )
                    })
                    .child(
                        Button::new("reveal")
                            .label("Show in folder")
                            .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
                    ),
            )
            .into_any_element()
    }
    fn workspace(&self, cx: &mut Context<Self>) -> AnyElement {
        let project = self.selected_project.and_then(|id| self.book.project(id));
        let cards = self
            .gallery
            .iter()
            .filter(|a| project.is_some_and(|p| p.name == a.project))
            .filter(|a| {
                self.selected_prompt
                    .is_none_or(|id| self.book.matches_prompt(a, id))
            })
            .map(|asset| self.card(asset, cx))
            .collect::<Vec<_>>();
        div().flex_1().min_h_0().flex().child(self.sidebar(cx))
            .child(div().flex_1().min_w_0().flex().flex_col()
                .child(div().id("gallery").flex_1().min_h_0().overflow_y_scroll().p_5().flex().flex_col().gap_4()
                    .child(div().flex().justify_between().items_center()
                        .child(div().text_2xl().font_weight(FontWeight::BOLD).child(project.map(|p| p.name.clone()).unwrap_or_else(|| "Your studio".into())))
                        .child(label(&format!("{} output(s)", cards.len()))))
                    .when(cards.is_empty(), |col| col.child(div().p_8().rounded_xl().border_1().border_color(rgb(0x30384a)).flex().flex_col().gap_3()
                        .child(div().text_xl().child(if project.is_some() { "Bring your next idea to life." } else { "Start with a project." }))
                        .child(label(if project.is_some() { "Write a prompt below, choose your output and model, then generate." } else { "Create or select a project from the left sidebar." }))))
                    .child(div().flex().flex_wrap().gap_4().children(cards)))
                .child(self.composer(cx)))
            .into_any_element()
    }
    fn folder_fields(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(label("Output folder"))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.folder).disabled(self.connecting)),
                    )
                    .child(
                        Button::new("browse")
                            .label("Browse…")
                            .disabled(self.folder_busy || self.connecting)
                            .on_click(cx.listener(Self::browse)),
                    ),
            )
            .child(
                Checkbox::new("project-folders")
                    .label("Create a folder for each project")
                    .checked(self.draft_settings.project_folders)
                    .disabled(self.connecting)
                    .on_click(cx.listener(|this, value, _, cx| {
                        this.draft_settings.project_folders = *value;
                        cx.notify();
                    })),
            )
            .child(
                Checkbox::new("date-folders")
                    .label("Group creations by date")
                    .checked(self.draft_settings.date_folders)
                    .disabled(self.connecting)
                    .on_click(cx.listener(|this, value, _, cx| {
                        this.draft_settings.date_folders = *value;
                        cx.notify();
                    })),
            )
            .child(div().text_xs().text_color(rgb(0x99a5bc)).child(
                "Projects, history, outputs and video jobs are indexed in _studio/studio.sqlite3.",
            ))
            .into_any_element()
    }
    fn settings_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .absolute()
            .inset_0()
            .bg(rgba(0x080b14ee))
            .flex()
            .items_center()
            .justify_center()
            .p_8()
            .child(
                div()
                    .id("settings-modal")
                    .w(px(610.))
                    .max_h_full()
                    .overflow_y_scroll()
                    .p_6()
                    .rounded_xl()
                    .bg(rgb(0x1b2130))
                    .border_1()
                    .border_color(rgb(0x3a455d))
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_2xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child("Settings"),
                            )
                            .child(Button::new("close-settings").label("Close ×").on_click(
                                cx.listener(|this, _, w, cx| this.close_settings(w, cx)),
                            )),
                    )
                    .child(self.connection_fields(cx))
                    .child(self.folder_fields(cx))
                    .when(self.error, |col| {
                        col.child(
                            div()
                                .text_sm()
                                .text_color(rgb(0xffaaa8))
                                .child(self.status.clone()),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("cancel-settings").label("Cancel").on_click(
                                    cx.listener(|this, _, w, cx| this.close_settings(w, cx)),
                                ),
                            )
                            .child(
                                Button::new("save-settings")
                                    .primary()
                                    .label("Save settings")
                                    .loading(self.connecting)
                                    .disabled(self.connecting)
                                    .on_click(cx.listener(Self::save_settings)),
                            ),
                    ),
            )
            .into_any_element()
    }
    fn image_viewer(&self, asset: &Asset, cx: &mut Context<Self>) -> AnyElement {
        let copy = asset.path.clone();
        let reveal = asset.path.clone();
        let edit = asset.path.clone();
        div()
            .absolute()
            .inset_0()
            .bg(rgba(0x080b14fa))
            .p_5()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_lg()
                            .child(asset.project.clone()),
                    )
                    .child(
                        Button::new("viewer-edit")
                            .label("Edit image")
                            .disabled(self.busy || self.active_provider() == Provider::Ollama)
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.begin_edit(edit.clone(), w, cx)
                            })),
                    )
                    .child(
                        Button::new("viewer-copy")
                            .label("Copy image")
                            .on_click(cx.listener(move |this, _, _, _| this.copy(copy.clone()))),
                    )
                    .child(
                        Button::new("viewer-reveal")
                            .label("Show in folder")
                            .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
                    )
                    .child(
                        Button::new("viewer-close")
                            .label("Close ×  ·  Esc")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.viewer = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div().flex_1().min_h_0().w_full().child(
                    img(asset.path.clone())
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                ),
            )
            .child(
                div()
                    .id("viewer-caption")
                    .max_h(px(96.))
                    .overflow_y_scroll()
                    .text_sm()
                    .child(asset.prompt.clone()),
            )
            .child(label(&asset.model))
            .into_any_element()
    }
}

fn provider_key(provider: Provider) -> String {
    std::env::var(if provider == Provider::Ollama {
        "OLLAMA_API_KEY"
    } else {
        "LITELLM_API_KEY"
    })
    .unwrap_or_default()
}

fn image_sizes() -> Vec<String> {
    [
        "1024x1024",
        "1536x1024",
        "1024x1536",
        "1792x1024",
        "1024x1792",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}
fn label(text: &str) -> Div {
    div()
        .text_sm()
        .text_color(rgb(0xa6aec4))
        .child(text.to_string())
}
fn shorten(text: &str, count: usize) -> String {
    let mut value: String = text.chars().take(count).collect();
    if text.chars().count() > count {
        value.push('…');
    }
    value
}

impl Render for Studio {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .px_6()
            .py_4()
            .border_b_1()
            .border_color(rgb(0x292e3d))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .bg(rgb(0xa3e0c0))
                            .text_color(rgb(0x101711))
                            .rounded_lg()
                            .px_3()
                            .py_2()
                            .font_weight(FontWeight::BOLD)
                            .child("R"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::BOLD)
                                    .child("RustLiLm Studio"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x99a5bc))
                                    .child("Your gateway. Your models. Your creations."),
                            ),
                    ),
            )
            .child(label(match self.page {
                Page::Gateway => "01 / Connect",
                Page::Output => "02 / Output",
                Page::Studio => "Studio",
            }));
        let body = match self.page {
            Page::Gateway => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w(px(550.))
                        .p_8()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .bg(rgb(0x1b2130))
                        .rounded_xl()
                        .child(
                            div()
                                .text_3xl()
                                .font_weight(FontWeight::BOLD)
                                .child("A home for your imagination."),
                        )
                        .child(label(
                            "Connect to LiteLLM to discover your available models.",
                        ))
                        .child(self.connection_fields(cx))
                        .child(
                            Button::new("connect")
                                .primary()
                                .label("Connect & discover models")
                                .loading(self.connecting)
                                .disabled(self.connecting)
                                .on_click(cx.listener(Self::connect)),
                        ),
                )
                .into_any_element(),
            Page::Output => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w(px(610.))
                        .p_6()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(
                            div()
                                .text_2xl()
                                .font_weight(FontWeight::BOLD)
                                .child("Make room for your ideas."),
                        )
                        .child(self.folder_fields(cx))
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .child(Button::new("back").label("Back").on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.page = Page::Gateway;
                                        cx.notify();
                                    },
                                )))
                                .child(
                                    Button::new("open-studio")
                                        .primary()
                                        .label("Open studio")
                                        .on_click(cx.listener(Self::enter_studio)),
                                ),
                        ),
                )
                .into_any_element(),
            Page::Studio => self.workspace(cx),
        };
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x10141d))
            .text_color(rgb(0xeff1f8))
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    if this.viewer.take().is_some() {
                        cx.stop_propagation();
                        cx.notify();
                    } else if this.jobs_open {
                        this.jobs_open = false;
                        cx.stop_propagation();
                        cx.notify();
                    } else if this.settings_open {
                        this.close_settings(window, cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .child(header)
            .child(body)
            .child(
                div()
                    .flex_shrink_0()
                    .px_5()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(0x292e3d))
                    .text_sm()
                    .text_color(if self.error {
                        rgb(0xffaaa8)
                    } else {
                        rgb(0xa6aec4)
                    })
                    .child(self.status.clone()),
            )
            .when(self.settings_open, |root| {
                root.child(self.settings_modal(cx))
            })
            .when(self.jobs_open, |root| root.child(self.jobs_modal(cx)))
            .when_some(self.viewer.as_ref(), |root, asset| {
                root.child(self.image_viewer(asset, cx))
            })
            .when_some(self.toast.as_ref(), |root, toast| {
                root.child(
                    div()
                        .absolute()
                        .top(px(84.))
                        .right_5()
                        .px_5()
                        .py_3()
                        .rounded_lg()
                        .bg(rgb(0xa3e0c0))
                        .text_color(rgb(0x101711))
                        .shadow_lg()
                        .child(format!("✓  {}", toast.message)),
                )
            })
    }
}
fn open_file(path: &std::path::Path) -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer.exe")
            .arg(path)
            .spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(path).spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).spawn()?;
    }
    Ok(())
}
fn main() {
    Application::new()
        .with_assets(gpui_component_assets::Assets)
        .run(|cx| {
            gpui_component::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1320.), px(900.)),
                    cx,
                ))),
                window_min_size: Some(size(px(1000.), px(760.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("RustLiLm Studio".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            if let Err(error) = cx.open_window(options, |window, cx| {
                let view = cx.new(|cx| Studio::new(window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            }) {
                eprintln!("Failed to open RustLiLm Studio: {error}");
                cx.quit();
            }
            cx.activate(true);
        });
}
