#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use gpui::{prelude::*, *};
use gpui_component::{
    button::*,
    checkbox::Checkbox,
    input::{Input, InputState},
    select::{Select, SelectState},
    Disableable, IndexPath, Root, Theme, ThemeMode,
};
use rustlilm_studio::{
    gateway::{Gateway, Generation, Model, PendingVideo},
    projects::{ProjectBook, PromptEntry, PromptState},
    storage::{self, Asset, MediaKind, Settings},
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
    model: Entity<SelectState<Vec<String>>>,
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
        let connected_key = std::env::var("LITELLM_API_KEY").unwrap_or_default();
        let (tx, rx) = mpsc::channel();
        let this = Self {
            url: input(
                settings.gateway_url.clone(),
                "https://your-gateway.example.com",
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
            model: cx.new(|cx| {
                SelectState::new(Vec::<String>::new(), None, window, cx).searchable(true)
            }),
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
        match Gateway::new(&self.url.read(cx).value(), &self.key.read(cx).value()) {
            Ok(gateway) => {
                self.connect_epoch += 1;
                let epoch = self.connect_epoch;
                self.connecting = true;
                self.error = false;
                self.status = "Fetching gateway models…".into();
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
            .filter(|m| m.supports(self.kind))
            .map(|m| m.id.clone())
            .collect::<Vec<_>>();
        let index = previous
            .as_ref()
            .and_then(|id| items.iter().position(|m| m == id))
            .or(if items.is_empty() { None } else { Some(0) })
            .map(IndexPath::new);
        self.model.update(cx, |select, cx| {
            select.set_items(items, window, cx);
            select.set_selected_index(index, window, cx);
        });
    }
    fn set_kind(&mut self, kind: MediaKind, window: &mut Window, cx: &mut Context<Self>) {
        self.kind = kind;
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
        settings.gateway_url = self.url.read(cx).value().trim().into();
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
            i.set_value(self.settings.gateway_url.clone(), window, cx)
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
        if settings.gateway_url == self.settings.gateway_url
            && self.key.read(cx).value().as_ref() == self.connected_key
        {
            match self.apply_workspace(settings) {
                Ok(()) => {
                    self.settings_open = false;
                    self.show_toast("Settings saved");
                    self.status = "Ready to create.".into();
                    self.error = false;
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
        self.set_kind(entry.kind, window, cx);
        self.model
            .update(cx, |s, cx| s.set_selected_value(&entry.model, window, cx));
        self.size
            .update(cx, |s, cx| s.set_selected_value(&entry.size, window, cx));
        self.seconds.update(cx, |s, cx| {
            s.set_selected_value(&entry.seconds.to_string(), window, cx)
        });
        if self.model.read(cx).selected_value().is_none() {
            self.fail(
                "This prompt's model is unavailable. Choose a model before generating.".into(),
            );
        } else {
            self.status = "Prompt restored. You can edit and generate again.".into();
            self.error = false;
        }
        cx.notify();
    }
    fn generate(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
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
                            gateway.images(&request)
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
        if self.busy {
            return;
        }
        let Some(gateway) = self.gateway.clone() else {
            return;
        };
        let tx = self.tx.clone();
        let logs = storage::metadata_dir(&self.settings.output);
        self.start_job(MediaKind::Video);
        let cancel = self.cancel.clone();
        std::thread::spawn(move || {
            let mut prompt_id = None;
            let result = (|| -> anyhow::Result<Vec<Asset>> {
                let path = rfd::FileDialog::new()
                    .set_title("Resume a saved video job")
                    .set_directory(logs.join("jobs"))
                    .add_filter("Video jobs", &["json"])
                    .pick_file()
                    .ok_or_else(|| anyhow::anyhow!("Resume cancelled"))?;
                let mut pending: PendingVideo = serde_json::from_slice(&std::fs::read(&path)?)?;
                pending.checkpoint = path;
                pending.request.logs = logs;
                prompt_id = pending.request.prompt_id;
                gateway
                    .poll_video(&pending, cancel, |s| {
                        let _ = tx.send(Event::Progress(s));
                    })
                    .map(|a| vec![a])
            })();
            let _ = tx.send(Event::Finished(
                prompt_id,
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
                        .disabled(self.busy)
                        .on_click(cx.listener(Self::open_settings)),
                ),
            )
            .into_any_element()
    }
    fn composer(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex_shrink_0()
            .p_5()
            .border_t_1()
            .border_color(rgb(0x292e3d))
            .bg(rgb(0x171c27))
            .flex()
            .gap_5()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(label("PROMPT"))
                            .child(
                                Button::new("new-prompt")
                                    .label("New prompt")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.selected_prompt = None;
                                        this.prompt.update(cx, |i, cx| i.set_value("", window, cx));
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        Input::new(&self.prompt)
                            .h(px(235.))
                            .disabled(self.busy || self.selected_project.is_none()),
                    ),
            )
            .child(
                div()
                    .w(px(260.))
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(label("OUTPUT"))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("images")
                                    .label("Image")
                                    .when(self.kind == MediaKind::Image, |b| b.primary())
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.set_kind(MediaKind::Image, w, cx)
                                    })),
                            )
                            .child(
                                Button::new("videos")
                                    .label("Video")
                                    .when(self.kind == MediaKind::Video, |b| b.primary())
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.set_kind(MediaKind::Video, w, cx)
                                    })),
                            ),
                    )
                    .child(label("Model"))
                    .child(Select::new(&self.model).disabled(self.busy))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .child(label("Size"))
                                    .child(Select::new(&self.size).disabled(self.busy)),
                            )
                            .when(self.kind == MediaKind::Video, |row| {
                                row.child(
                                    div()
                                        .w(px(80.))
                                        .child(label("Seconds"))
                                        .child(Select::new(&self.seconds).disabled(self.busy)),
                                )
                            }),
                    )
                    .child(
                        Button::new("generate")
                            .primary()
                            .label(if self.kind == MediaKind::Image {
                                "Generate image"
                            } else {
                                "Generate video"
                            })
                            .loading(self.busy)
                            .disabled(self.busy || self.selected_project.is_none())
                            .on_click(cx.listener(Self::generate)),
                    )
                    .when(self.kind == MediaKind::Video, |col| {
                        col.child(
                            Button::new("resume")
                                .label("Resume saved video…")
                                .disabled(self.busy)
                                .on_click(cx.listener(Self::resume)),
                        )
                    })
                    .when(self.busy && self.running_video, |col| {
                        col.child(Button::new("stop").label("Stop waiting").on_click(
                            cx.listener(|this, _, _, _| this.cancel.store(true, Ordering::Relaxed)),
                        ))
                    }),
            )
            .into_any_element()
    }
    fn card(&self, asset: &Asset, cx: &mut Context<Self>) -> AnyElement {
        let reveal = asset.path.clone();
        let copy = asset.path.clone();
        let preview = asset.clone();
        let video = asset.path.clone();
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
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x99a5bc))
                    .child("JSON metadata and video jobs are kept separately in _studio."),
            )
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
                    .child(label("Gateway URL"))
                    .child(Input::new(&self.url).disabled(self.connecting))
                    .child(label("API key"))
                    .child(Input::new(&self.key).disabled(self.connecting))
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
        let body =
            match self.page {
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
                            .child(label("Gateway URL"))
                            .child(Input::new(&self.url).disabled(self.connecting))
                            .child(label("API key"))
                            .child(Input::new(&self.key).disabled(self.connecting))
                            .child(div().text_xs().text_color(rgb(0x99a5bc)).child(
                                "Your key stays in memory. You can also set LITELLM_API_KEY.",
                            ))
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
