use base64::{engine::general_purpose::STANDARD, Engine};
use rustlilm_studio::{
    gateway::{Gateway, Generation},
    storage::{self, MediaKind, Settings},
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
                let mut body = String::new();
                req.as_reader().read_to_string(&mut body).unwrap();
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
    assert!(assets[0].path.with_extension("json").exists());
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
    assert!(dir.path().join("pending-video.json").exists());
    let asset = gateway
        .poll_video(&pending, Arc::new(AtomicBool::new(false)), |_| {})
        .unwrap();
    assert_eq!(std::fs::read(asset.path).unwrap(), video);
    assert!(!dir.path().join("pending-video.json").exists());
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
    assert!(dir.path().join("pending-video.json").exists());
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
        &png(),
        "png",
        MediaKind::Image,
        "model",
        "prompt",
        "project",
    )
    .unwrap();
    let second = storage::save_asset(
        dir.path(),
        &png(),
        "png",
        MediaKind::Image,
        "model",
        "prompt",
        "project",
    )
    .unwrap();
    assert_ne!(first.path, second.path);
}
