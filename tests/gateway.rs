use base64::{engine::general_purpose::STANDARD, Engine};
use rustlilm_studio::{
    gateway::{Gateway, Generation},
    storage::{self, AssetDetails, MediaKind, Settings},
};
use serde_json::json;
use std::{
    io::Cursor,
    sync::{atomic::AtomicBool, Arc},
    thread,
    time::Duration,
};
use tiny_http::{Header, Response, Server};

struct Step {
    method: &'static str,
    path: String,
    status: u16,
    body: Vec<u8>,
    auth: Option<&'static str>,
    contains: Option<&'static str>,
}
fn step(method: &'static str, path: &str, body: serde_json::Value) -> Step {
    Step {
        method,
        path: path.into(),
        status: 200,
        body: serde_json::to_vec(&body).unwrap(),
        auth: Some("Bearer test-key"),
        contains: None,
    }
}
fn mock(steps: Vec<Step>) -> (String, thread::JoinHandle<()>) {
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr());
    let handle = thread::spawn(move || {
        for step in steps {
            let mut req = server
                .recv_timeout(Duration::from_secs(15))
                .unwrap()
                .expect("Expected gateway request");
            assert_eq!(req.method().as_str(), step.method);
            assert_eq!(req.url(), step.path);
            assert_eq!(
                req.headers()
                    .iter()
                    .find(|h| h.field.equiv("Authorization"))
                    .map(|h| h.value.as_str()),
                step.auth
            );
            if let Some(contains) = step.contains {
                let mut bytes = Vec::new();
                req.as_reader().read_to_end(&mut bytes).unwrap();
                let body = String::from_utf8_lossy(&bytes);
                assert!(
                    body.contains(contains),
                    "Request did not contain {contains}"
                );
            }
            req.respond(
                Response::from_data(step.body)
                    .with_status_code(step.status)
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap()),
            )
            .unwrap();
        }
    });
    (url, handle)
}
fn generation(dir: &std::path::Path) -> Generation {
    Generation {
        model: "my-image-alias".into(),
        prompt: "a lighthouse".into(),
        project: "Test".into(),
        kind: MediaKind::Image,
        size: "1024x1024".into(),
        seconds: 4,
        destination: dir.into(),
        input_image: None,
        logs: storage::metadata_dir(dir),
        prompt_id: Some(uuid::Uuid::new_v4()),
    }
}
fn png() -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

#[test]
fn normalizes_v1_and_preserves_gateway_prefix() {
    let gateway = Gateway::new("https://example.com/litellm/v1/", "").unwrap();
    assert_eq!(
        gateway.endpoint("v1/models").unwrap().as_str(),
        "https://example.com/litellm/v1/models"
    );
}
#[test]
fn rejects_credentials_and_unsafe_url_schemes() {
    for url in [
        "file:///tmp/foo",
        "https://user:secret@example.com",
        "https://example.com?key=secret",
        "https://example.com#secret",
        "garbage",
    ] {
        assert!(Gateway::new(url, "").is_err());
    }
}
#[test]
fn discovers_models_and_enriches_modes() {
    let (url, server) = mock(vec![
        step(
            "GET",
            "/v1/models",
            json!({"data":[{"id":"z"}, {"id":"a"}, {"id":"a"}]}),
        ),
        step(
            "GET",
            "/model/info",
            json!({"data":[{"model_name":"a","model_info":{"mode":"image_generation"}}]}),
        ),
    ]);
    let models = Gateway::new(&url, "test-key").unwrap().models().unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "a");
    assert!(models[0].supports(MediaKind::Image));
    assert!(!models[0].supports(MediaKind::Video));
    assert!(models[1].supports(MediaKind::Video));
    server.join().unwrap();
}
#[test]
fn capability_endpoint_permission_is_optional() {
    let mut denied = step("GET", "/model/info", json!({"error":"forbidden"}));
    denied.status = 403;
    let (url, server) = mock(vec![
        step("GET", "/v1/models", json!({"data":[{"id":"alias"}]})),
        denied,
    ]);
    assert_eq!(
        Gateway::new(&url, "test-key").unwrap().models().unwrap()[0].id,
        "alias"
    );
    server.join().unwrap();
}
#[test]
fn auth_errors_redact_api_key() {
    let mut denied = step("GET", "/v1/models", json!({"error":"bad test-key"}));
    denied.status = 401;
    let (url, server) = mock(vec![denied]);
    let error = Gateway::new(&url, "test-key")
        .unwrap()
        .models()
        .unwrap_err()
        .to_string();
    assert!(error.contains("401"));
    assert!(!error.contains("test-key"));
    server.join().unwrap();
}
#[test]
fn base64_images_are_saved_with_metadata_and_gallery() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = png();
    let mut post = step(
        "POST",
        "/v1/images/generations",
        json!({"data":[{"b64_json":STANDARD.encode(&bytes)}]}),
    );
    post.contains = Some("a lighthouse");
    let (url, server) = mock(vec![post]);
    let assets = Gateway::new(&url, "test-key")
        .unwrap()
        .images(&generation(dir.path()))
        .unwrap();
    assert_eq!(std::fs::read(&assets[0].path).unwrap(), bytes);
    assert!(!assets[0].path.with_extension("json").exists());
    assert!(storage::metadata_dir(dir.path())
        .join("studio.sqlite3")
        .exists());
    storage::save_gallery(dir.path(), &assets).unwrap();
    assert_eq!(
        storage::load_gallery(dir.path()).unwrap()[0].prompt,
        "a lighthouse"
    );
    server.join().unwrap();
}
#[test]
fn external_download_never_receives_gateway_key() {
    let bytes = png();
    let (cdn, cdn_server) = mock(vec![Step {
        method: "GET",
        path: "/image.png".into(),
        status: 200,
        body: bytes.clone(),
        auth: None,
        contains: None,
    }]);
    let (url, server) = mock(vec![step(
        "POST",
        "/v1/images/generations",
        json!({"data":[{"url":format!("{cdn}/image.png")}]}),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let assets = Gateway::new(&url, "test-key")
        .unwrap()
        .images(&generation(dir.path()))
        .unwrap();
    assert_eq!(std::fs::read(&assets[0].path).unwrap(), bytes);
    server.join().unwrap();
    cdn_server.join().unwrap();
}
#[test]
fn invalid_images_are_not_written() {
    let (url, server) = mock(vec![step(
        "POST",
        "/v1/images/generations",
        json!({"data":[{"b64_json":STANDARD.encode("not an image")}]}),
    )]);
    let dir = tempfile::tempdir().unwrap();
    assert!(Gateway::new(&url, "test-key")
        .unwrap()
        .images(&generation(dir.path()))
        .is_err());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    server.join().unwrap();
}
#[test]
fn empty_image_results_are_errors() {
    let (url, server) = mock(vec![step(
        "POST",
        "/v1/images/generations",
        json!({"data":[]}),
    )]);
    let dir = tempfile::tempdir().unwrap();
    assert!(Gateway::new(&url, "test-key")
        .unwrap()
        .images(&generation(dir.path()))
        .is_err());
    server.join().unwrap();
}
#[test]
fn video_submission_checkpoint_polling_and_download() {
    let video = b"\0\0\0\x18ftypisom00000000".to_vec();
    let mut post = step(
        "POST",
        "/v1/videos",
        json!({"id":"video_123","status":"queued"}),
    );
    post.contains = Some("name=\"model\"");
    let (url, server) = mock(vec![
        post,
        step(
            "GET",
            "/v1/videos/video_123",
            json!({"status":"completed","progress":100}),
        ),
        Step {
            method: "GET",
            path: "/v1/videos/video_123/content".into(),
            status: 200,
            body: video.clone(),
            auth: Some("Bearer test-key"),
            contains: None,
        },
    ]);
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::new(&url, "test-key").unwrap();
    let mut request = generation(dir.path());
    request.kind = MediaKind::Video;
    let pending = gateway.submit_video(&request).unwrap();
    assert_eq!(
        rustlilm_studio::database::Database::workspace(&request.logs)
            .unwrap()
            .jobs()
            .unwrap()
            .len(),
        1
    );
    let asset = gateway
        .poll_video(&pending, Arc::new(AtomicBool::new(false)), |_| {})
        .unwrap();
    assert_eq!(std::fs::read(asset.path).unwrap(), video);
    assert!(
        rustlilm_studio::database::Database::workspace(&request.logs)
            .unwrap()
            .jobs()
            .unwrap()
            .is_empty()
    );
    server.join().unwrap();
}
#[test]
fn cancelled_polling_preserves_checkpoint() {
    let (url, server) = mock(vec![step("POST", "/v1/videos", json!({"id":"video_123"}))]);
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::new(&url, "test-key").unwrap();
    let pending = gateway.submit_video(&generation(dir.path())).unwrap();
    assert!(gateway
        .poll_video(&pending, Arc::new(AtomicBool::new(true)), |_| {})
        .is_err());
    assert_eq!(
        rustlilm_studio::database::Database::workspace(&pending.request.logs)
            .unwrap()
            .jobs()
            .unwrap()
            .len(),
        1
    );
    server.join().unwrap();
}
#[test]
fn sanitizes_project_names_on_windows_and_unix() {
    assert_eq!(storage::safe_component("../../escape"), "escape");
    assert_eq!(storage::safe_component("CON"), "project_CON");
    assert_eq!(storage::safe_component("   "), "Untitled");
    assert_eq!(storage::safe_component("My project"), "My_project");
}
#[test]
fn output_grouping_and_unique_names() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = Settings {
        output: dir.path().into(),
        project_folders: true,
        date_folders: true,
        ..Settings::default()
    };
    let grouped = settings.destination("../Example").unwrap();
    assert!(grouped.starts_with(dir.path().join("Example")));
    assert_eq!(grouped.file_name().unwrap().to_str().unwrap().len(), 10);
    settings.project_folders = false;
    settings.date_folders = false;
    assert_eq!(settings.destination("ignored").unwrap(), dir.path());
    let first = storage::save_asset(
        dir.path(),
        &storage::metadata_dir(dir.path()),
        &png(),
        "png",
        AssetDetails {
            kind: MediaKind::Image,
            model: "model",
            prompt: "prompt",
            project: "project",
            prompt_id: None,
        },
    )
    .unwrap();
    let second = storage::save_asset(
        dir.path(),
        &storage::metadata_dir(dir.path()),
        &png(),
        "png",
        AssetDetails {
            kind: MediaKind::Image,
            model: "model",
            prompt: "prompt",
            project: "project",
            prompt_id: None,
        },
    )
    .unwrap();
    assert_ne!(first.path, second.path);
}

#[test]
fn edits_upload_reference_image_and_keep_original_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("original.png");
    let bytes = png();
    std::fs::write(&original, &bytes).unwrap();
    let mut post = step(
        "POST",
        "/v1/images/edits",
        json!({"data":[{"b64_json":STANDARD.encode(&bytes)}]}),
    );
    post.contains = Some("name=\"image\"");
    let (url, server) = mock(vec![post]);
    let mut request = generation(dir.path());
    request.input_image = Some(original.clone());
    let assets = Gateway::new(&url, "test-key")
        .unwrap()
        .images(&request)
        .unwrap();
    assert_ne!(assets[0].path, original);
    assert_eq!(std::fs::read(original).unwrap(), bytes);
    assert_eq!(assets[0].prompt_id, request.prompt_id);
    server.join().unwrap();
}

#[test]
fn invalid_edit_reference_fails_before_sending_a_request() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.png");
    std::fs::write(&path, b"not an image").unwrap();
    let mut request = generation(dir.path());
    request.input_image = Some(path);
    let error = Gateway::new("http://127.0.0.1:1", "secret")
        .unwrap()
        .images(&request)
        .unwrap_err()
        .to_string();
    assert!(error.contains("decode"));
}

fn ollama_step(method: &'static str, path: &str, body: serde_json::Value) -> Step {
    let mut s = step(method, path, body);
    s.auth = None;
    s
}
fn ollama_stream(path: &str, lines: &[serde_json::Value]) -> Step {
    let mut s = ollama_step("POST", path, json!(null));
    s.body = lines
        .iter()
        .map(|v| format!("{v}\n"))
        .collect::<String>()
        .into_bytes();
    s
}
fn ollama(url: &str) -> Gateway {
    Gateway::with_provider(url, "", storage::Provider::Ollama).unwrap()
}

#[test]
fn ollama_discovery_combines_verified_catalog_with_installed_generation_models() {
    let (url, server) = mock(vec![ollama_step(
        "GET",
        "/api/tags",
        json!({"models":[
        {"name":"x/z-image-turbo:latest","capabilities":["image"]},
        {"name":"local-image:latest","capabilities":["image"]},
        {"name":"vision-chat:latest","capabilities":["vision","completion"]}]}),
    )]);
    let models = ollama(&url).models().unwrap();
    assert!(models
        .iter()
        .any(|m| m.id == "x/z-image-turbo:latest" && m.downloaded));
    assert!(models
        .iter()
        .any(|m| m.id == "x/flux2-klein:latest" && !m.downloaded && m.source.is_some()));
    assert!(models.iter().any(|m| m.id == "local-image:latest"));
    assert!(!models.iter().any(|m| m.id.contains("vision-chat")));
    assert!(!models
        .iter()
        .any(|m| m.supports(MediaKind::Video) || m.supports_operation(MediaKind::Image, true)));
    server.join().unwrap();
}

#[test]
fn ollama_pull_streams_progress_and_requires_success() {
    let (url, server) = mock(vec![
        ollama_step("GET", "/api/tags", json!({"models":[]})),
        ollama_stream(
            "/api/pull",
            &[
                json!({"status":"pulling manifest"}),
                json!({"status":"pulling layer","completed":50,"total":100}),
                json!({"status":"success"}),
            ],
        ),
    ]);
    let mut progress = Vec::new();
    ollama(&url)
        .pull_ollama("x/z-image-turbo", Arc::new(AtomicBool::new(false)), |s| {
            progress.push(s)
        })
        .unwrap();
    assert!(progress.iter().any(|s| s.contains("50.0%")));
    server.join().unwrap();
}

#[test]
fn ollama_does_not_download_an_installed_model() {
    let (url, server) = mock(vec![ollama_step(
        "GET",
        "/api/tags",
        json!({"models":[{"name":"x/z-image-turbo:latest"}]}),
    )]);
    ollama(&url)
        .pull_ollama("x/z-image-turbo", Arc::new(AtomicBool::new(false)), |_| {})
        .unwrap();
    server.join().unwrap();
}

#[test]
fn ollama_incomplete_download_is_not_treated_as_installed() {
    let (url, server) = mock(vec![
        ollama_step("GET", "/api/tags", json!({"models":[]})),
        ollama_stream(
            "/api/pull",
            &[json!({"status":"pulling layer","completed":50,"total":100})],
        ),
    ]);
    let error = ollama(&url)
        .pull_ollama("x/z-image-turbo", Arc::new(AtomicBool::new(false)), |_| {})
        .unwrap_err()
        .to_string();
    assert!(error.contains("before reporting success"));
    server.join().unwrap();
}

#[test]
fn ollama_stream_errors_do_not_expose_api_keys() {
    let mut tags = ollama_step("GET", "/api/tags", json!({"models":[]}));
    tags.auth = Some("Bearer test-key");
    let mut stream = ollama_stream("/api/pull", &[json!({"error":"test-key was refused"})]);
    stream.auth = Some("Bearer test-key");
    let (url, server) = mock(vec![tags, stream]);
    let error = Gateway::with_provider(&url, "test-key", storage::Provider::Ollama)
        .unwrap()
        .pull_ollama("x/z-image-turbo", Arc::new(AtomicBool::new(false)), |_| {})
        .unwrap_err()
        .to_string();
    assert!(!error.contains("test-key"));
    assert!(error.contains("redacted"));
    server.join().unwrap();
}

#[test]
fn ollama_native_image_stream_is_saved_and_progress_reported() {
    let mut stream = ollama_stream(
        "/api/generate",
        &[
            json!({"completed":1,"total":4,"done":false}),
            json!({"image":STANDARD.encode(png()),"done":true}),
        ],
    );
    stream.contains = Some("\"width\":1024");
    let (url, server) = mock(vec![
        ollama_step(
            "GET",
            "/api/tags",
            json!({"models":[{"name":"my-image-alias:latest"}]}),
        ),
        stream,
    ]);
    let dir = tempfile::tempdir().unwrap();
    let mut progress = Vec::new();
    let assets = ollama(&url)
        .ollama_images(
            &generation(dir.path()),
            Arc::new(AtomicBool::new(false)),
            |s| progress.push(s),
        )
        .unwrap();
    assert_eq!(std::fs::read(&assets[0].path).unwrap(), png());
    assert!(progress.iter().any(|s| s.contains("1/4")));
    server.join().unwrap();
}

#[test]
fn ollama_text_response_is_not_mistaken_for_an_image() {
    let (url, server) = mock(vec![
        ollama_step(
            "GET",
            "/api/tags",
            json!({"models":[{"name":"my-image-alias:latest"}]}),
        ),
        ollama_stream(
            "/api/generate",
            &[json!({"response":"Here is an image description","done":true})],
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    assert!(ollama(&url)
        .ollama_images(
            &generation(dir.path()),
            Arc::new(AtomicBool::new(false)),
            |_| {}
        )
        .is_err());
    server.join().unwrap();
}

#[test]
fn ollama_video_and_editing_fail_locally_with_a_clear_explanation() {
    let dir = tempfile::tempdir().unwrap();
    let client = ollama("http://127.0.0.1:1");
    let mut request = generation(dir.path());
    request.kind = MediaKind::Video;
    assert!(client
        .submit_video(&request)
        .unwrap_err()
        .to_string()
        .contains("video-generation"));
    request.kind = MediaKind::Image;
    request.input_image = Some(dir.path().join("reference.png"));
    assert!(client
        .ollama_images(&request, Arc::new(AtomicBool::new(false)), |_| {})
        .unwrap_err()
        .to_string()
        .contains("image editing"));
}
