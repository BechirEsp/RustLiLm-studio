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
    storage::{self, Asset, MediaKind, Settings},
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    time::Duration,
};

enum Event {
    Models(Result<Vec<Model>, String>),
    Folder(Option<PathBuf>),
    Progress(String),
    Finished(Result<Vec<Asset>, String>),
    Clipboard(Result<Vec<u8>, String>),
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Gateway,
    Output,
    Studio,
}

struct Studio {
    settings: Settings,
    page: Page,
    url: Entity<InputState>,
    key: Entity<InputState>,
    folder: Entity<InputState>,
    project: Entity<InputState>,
    prompt: Entity<InputState>,
    models: Vec<Model>,
    model: Entity<SelectState<Vec<String>>>,
    size: Entity<SelectState<Vec<String>>>,
    seconds: Entity<SelectState<Vec<String>>>,
    kind: MediaKind,
    gateway: Option<Gateway>,
    gallery: Vec<Asset>,
    busy: bool,
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
            Ok(settings) => (
                settings,
                "Connect your gateway to get started.".into(),
                false,
            ),
            Err(e) => (
                Settings::default(),
                format!("Could not load saved settings: {e}"),
                true,
            ),
        };
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
                state.set_value(
                    std::env::var("LITELLM_API_KEY").unwrap_or_default(),
                    window,
                    cx,
                );
                state
            }),
            folder: input(
                settings.output.to_string_lossy().into_owned(),
                "Output directory",
                window,
                cx,
            ),
            project: input("My project".into(), "Project name", window, cx),
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
            settings,
            page: Page::Gateway,
            models: Vec::new(),
            kind: MediaKind::Image,
            gateway: None,
            gallery: Vec::new(),
            busy: false,
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
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Models(result) => {
                    self.busy = false;
                    match result {
                        Ok(models) => {
                            self.models = models;
                            self.update_models(window, cx);
                            self.status =
                                format!("Connected · {} models available", self.models.len());
                            self.error = false;
                            self.page = Page::Output;
                        }
                        Err(e) => self.fail(e),
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
                Event::Progress(message) => {
                    self.status = message;
                }
                Event::Finished(result) => {
                    self.busy = false;
                    match result {
                        Ok(assets) => {
                            let count = assets.len();
                            for asset in assets {
                                self.gallery.insert(0, asset);
                            }
                            match storage::save_gallery(&self.settings.output, &self.gallery) {
                                Ok(()) => {
                                    self.status =
                                        format!("Saved {count} result(s) to your output folder.");
                                    self.error = false;
                                }
                                Err(e) => self.fail(format!(
                                    "Files saved, but gallery index could not be saved: {e}"
                                )),
                            }
                        }
                        Err(e) => self.fail(e),
                    }
                }
                Event::Clipboard(result) => match result {
                    Ok(bytes) => {
                        cx.write_to_clipboard(ClipboardItem::new_image(&gpui::Image::from_bytes(
                            gpui::ImageFormat::Png,
                            bytes,
                        )));
                        self.status = "Image copied to clipboard.".into();
                        self.error = false;
                    }
                    Err(e) => self.fail(e),
                },
            }
            cx.notify();
        }
    }

    fn fail(&mut self, error: String) {
        self.status = error;
        self.error = true;
    }

    fn connect(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match Gateway::new(&self.url.read(cx).value(), &self.key.read(cx).value()) {
            Ok(gateway) => {
                self.gateway = Some(gateway.clone());
                self.busy = true;
                self.error = false;
                self.status = "Fetching gateway models…".into();
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(Event::Models(gateway.models().map_err(|e| e.to_string())));
                });
            }
            Err(e) => self.fail(e.to_string()),
        }
        cx.notify();
    }

    fn update_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = self
            .models
            .iter()
            .filter(|m| m.supports(self.kind))
            .map(|m| m.id.clone())
            .collect::<Vec<_>>();
        let selection = if items.is_empty() {
            None
        } else {
            Some(IndexPath::new(0))
        };
        self.model.update(cx, |select, cx| {
            select.set_items(items, window, cx);
            select.set_selected_index(selection, window, cx);
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

    fn enter_studio(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.settings.gateway_url = self.url.read(cx).value().trim().into();
        self.settings.output = PathBuf::from(self.folder.read(cx).value().trim());
        let result = (|| -> anyhow::Result<Vec<Asset>> {
            if self.model.read(cx).selected_value().is_none() {
                anyhow::bail!("Choose a model first");
            }
            self.settings.destination(&self.project.read(cx).value())?;
            self.settings.save()?;
            storage::load_gallery(&self.settings.output)
        })();
        match result {
            Ok(gallery) => {
                self.gallery = gallery;
                self.page = Page::Studio;
                self.error = false;
                self.status = "Ready to create.".into();
            }
            Err(e) => self.fail(e.to_string()),
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
            let project = self.project.read(cx).value().to_string();
            Ok(Generation {
                model,
                prompt,
                destination: self.settings.destination(&project)?,
                project,
                kind: self.kind,
                size: self
                    .size
                    .read(cx)
                    .selected_value()
                    .cloned()
                    .unwrap_or_else(|| "1024x1024".into()),
                seconds: self
                    .seconds
                    .read(cx)
                    .selected_value()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(4),
            })
        })();
        match result {
            Ok(request) => {
                if request.kind == MediaKind::Video
                    && request.destination.join("pending-video.json").exists()
                {
                    self.fail("There is a saved video job in this project/date folder. Use Resume video first, or choose another project.".into());
                    cx.notify();
                    return;
                }
                self.start_job();
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
                                .map(|asset| vec![asset])
                        }
                    })();
                    let _ = tx.send(Event::Finished(result.map_err(|e| e.to_string())));
                });
            }
            Err(e) => self.fail(e.to_string()),
        }
        cx.notify();
    }

    fn start_job(&mut self) {
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
        self.start_job();
        let cancel = self.cancel.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<Vec<Asset>> {
                let path = rfd::FileDialog::new()
                    .set_title("Resume a pending-video.json job")
                    .add_filter("Saved video job", &["json"])
                    .pick_file()
                    .ok_or_else(|| anyhow::anyhow!("Resume cancelled"))?;
                let mut pending: PendingVideo = serde_json::from_slice(&std::fs::read(&path)?)?;
                // The selected job file's location is authoritative if its folder was moved.
                pending.request.destination = path.parent().unwrap().to_path_buf();
                gateway
                    .poll_video(&pending, cancel, |s| {
                        let _ = tx.send(Event::Progress(s));
                    })
                    .map(|a| vec![a])
            })();
            let _ = tx.send(Event::Finished(result.map_err(|e| e.to_string())));
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

    fn card(&self, asset: &Asset, cx: &mut Context<Self>) -> AnyElement {
        let path = asset.path.clone();
        let reveal = path.clone();
        let open = path.clone();
        let copy = path.clone();
        div()
            .id(SharedString::from(asset.id.to_string()))
            .group("asset")
            .relative()
            .w(px(292.))
            .rounded_xl()
            .overflow_hidden()
            .bg(rgb(0x191c25))
            .border_1()
            .border_color(rgb(0x2a2e3b))
            .child(if asset.kind == MediaKind::Image {
                img(path)
                    .w_full()
                    .h(px(220.))
                    .object_fit(ObjectFit::Contain)
                    .into_any_element()
            } else {
                div()
                    .h(px(220.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_3xl()
                    .child("▶  MP4")
                    .into_any_element()
            })
            .child(
                div()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .overflow_hidden()
                            .child(asset.prompt.clone()),
                    )
                    .child(div().text_xs().text_color(rgb(0x9ba2b7)).child(format!(
                            "{} · {}",
                            asset.model,
                            asset
                                .created
                                .with_timezone(&chrono::Local)
                                .format("%b %d · %H:%M")
                        ))),
            )
            .child(
                div()
                    .absolute()
                    .top_2()
                    .right_2()
                    .flex()
                    .gap_1()
                    .opacity(0.)
                    .group_hover("asset", |style| style.opacity(1.))
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
                    )
                    .when(asset.kind == MediaKind::Video, |row| {
                        row.child(Button::new("play").label("Play").on_click(move |_, _, _| {
                            let _ = open_file(&open);
                        }))
                    }),
            )
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

impl Render for Studio {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .px_8()
            .py_5()
            .border_b_1()
            .border_color(rgb(0x252936))
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
                                    .text_color(rgb(0x909ab2))
                                    .child("Your gateway. Your models. Your creations."),
                            ),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0xa3e0c0))
                    .child(match self.page {
                        Page::Gateway => "01 / Connect",
                        Page::Output => "02 / Configure",
                        Page::Studio => "Studio",
                    }),
            );
        let body = match self.page {
            Page::Gateway => {
                div()
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
                            .bg(rgb(0x191c25))
                            .rounded_xl()
                            .child(
                                div()
                                    .text_3xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child("A home for your imagination."),
                            )
                            .child(label(
                                "Connect to LiteLLM to discover the models available to your key.",
                            ))
                            .child(label("Gateway URL"))
                            .child(Input::new(&self.url).disabled(self.busy))
                            .child(label("API key"))
                            .child(Input::new(&self.key).disabled(self.busy))
                            .child(div().text_xs().text_color(rgb(0x909ab2)).child(
                                "The key stays in memory. You can also set LITELLM_API_KEY.",
                            ))
                            .child(
                                Button::new("connect")
                                    .primary()
                                    .label("Connect & discover models")
                                    .loading(self.busy)
                                    .disabled(self.busy)
                                    .on_click(cx.listener(Self::connect)),
                            ),
                    )
                    .into_any_element()
            }
            Page::Output => div()
                .id("setup-scroll")
                .flex_1()
                .overflow_y_scroll()
                .flex()
                .justify_center()
                .p_8()
                .child(
                    div()
                        .w(px(620.))
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(
                            div()
                                .text_2xl()
                                .font_weight(FontWeight::BOLD)
                                .child("Make room for your ideas."),
                        )
                        .child(label("Choose a model"))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(Button::new("setup-images").label("Images").on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.set_kind(MediaKind::Image, window, cx)
                                    }),
                                ))
                                .child(Button::new("setup-videos").label("Videos").on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.set_kind(MediaKind::Video, window, cx)
                                    }),
                                )),
                        )
                        .child(Select::new(&self.model))
                        .child(label("Output folder"))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(div().flex_1().child(Input::new(&self.folder)))
                                .child(
                                    Button::new("browse")
                                        .label("Browse…")
                                        .disabled(self.folder_busy)
                                        .on_click(cx.listener(Self::browse)),
                                ),
                        )
                        .child(label("Project name"))
                        .child(Input::new(&self.project))
                        .child(
                            Checkbox::new("project-folders")
                                .label("Create a folder for each project")
                                .checked(self.settings.project_folders)
                                .on_click(cx.listener(|this, checked, _, cx| {
                                    this.settings.project_folders = *checked;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Checkbox::new("date-folders")
                                .label("Group creations by date")
                                .checked(self.settings.date_folders)
                                .on_click(cx.listener(|this, checked, _, cx| {
                                    this.settings.date_folders = *checked;
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(Button::new("back").label("Back").on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.page = Page::Gateway;
                                        cx.notify();
                                    },
                                )))
                                .child(
                                    Button::new("start")
                                        .primary()
                                        .label("Open studio")
                                        .on_click(cx.listener(Self::enter_studio)),
                                ),
                        ),
                )
                .into_any_element(),
            Page::Studio => {
                let cards = self
                    .gallery
                    .iter()
                    .map(|asset| self.card(asset, cx))
                    .collect::<Vec<_>>();
                div().flex_1().min_h_0().flex()
                    .child(div().id("controls").w(px(350.)).flex_shrink_0().overflow_y_scroll().p_5().border_r_1().border_color(rgb(0x252936)).flex().flex_col().gap_4()
                        .child(label("CREATE"))
                        .child(div().flex().gap_2()
                            .child(Button::new("images").label("Images").disabled(self.busy).on_click(cx.listener(|this, _, window, cx| this.set_kind(MediaKind::Image, window, cx))))
                            .child(Button::new("videos").label("Videos").disabled(self.busy).on_click(cx.listener(|this, _, window, cx| this.set_kind(MediaKind::Video, window, cx)))))
                        .child(label(if self.kind == MediaKind::Image { "Image model" } else { "Video model" }))
                        .child(Select::new(&self.model).disabled(self.busy))
                        .child(div().text_xs().text_color(rgb(0x909ab2)).child("Aliases without capability metadata appear in both modes."))
                        .child(label("Project")).child(Input::new(&self.project).disabled(self.busy))
                        .child(label("Prompt")).child(Input::new(&self.prompt).h(px(180.)).disabled(self.busy))
                        .child(label("Size")).child(Select::new(&self.size).disabled(self.busy))
                        .when(self.kind == MediaKind::Video, |col| col.child(label("Duration (seconds)")).child(Select::new(&self.seconds).disabled(self.busy)))
                        .child(Button::new("generate").primary().label(if self.kind == MediaKind::Image { "Generate image" } else { "Generate video" })
                            .loading(self.busy).disabled(self.busy).on_click(cx.listener(Self::generate)))
                        .when(self.kind == MediaKind::Video, |col| col
                            .child(Button::new("resume").label("Resume video…").disabled(self.busy).on_click(cx.listener(Self::resume))))
                        .when(self.busy && self.kind == MediaKind::Video, |col| col.child(Button::new("stop").label("Stop waiting")
                            .on_click(cx.listener(|this, _, _, _| { this.cancel.store(true, Ordering::Relaxed); }))))
                        .child(Button::new("settings").label("Gateway & output settings").disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| { this.page = Page::Gateway; cx.notify(); }))))
                    .child(div().id("gallery").flex_1().min_w_0().overflow_y_scroll().p_6().flex().flex_col().gap_5()
                        .child(div().flex().justify_between().items_center()
                            .child(div().text_2xl().font_weight(FontWeight::BOLD).child("Your creations"))
                            .child(div().text_sm().text_color(rgb(0x909ab2)).child(format!("{} saved", self.gallery.len()))))
                        .when(cards.is_empty(), |col| col.child(div().p_8().rounded_xl().border_1().border_color(rgb(0x303645)).flex().flex_col().gap_3()
                            .child(div().text_xl().child("Start with a spark."))
                            .child(label("Write a prompt and generate your first image or video. Your creations will appear here."))))
                        .child(div().flex().flex_wrap().gap_4().children(cards)))
                    .into_any_element()
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x11141c))
            .text_color(rgb(0xeff1f8))
            .child(header)
            .child(body)
            .child(
                div()
                    .px_6()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(0x252936))
                    .text_sm()
                    .text_color(if self.error {
                        rgb(0xffaaa8)
                    } else {
                        rgb(0xa6aec4)
                    })
                    .child(self.status.clone()),
            )
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
                    size(px(1280.), px(850.)),
                    cx,
                ))),
                window_min_size: Some(size(px(960.), px(700.))),
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
