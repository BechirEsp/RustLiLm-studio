use chrono::Utc;
use rustlilm_studio::{
    credentials,
    database::Database,
    projects::{ProjectBook, PromptEntry, PromptState},
    storage::{self, MediaKind, Provider, Settings},
};
use std::fs;
use uuid::Uuid;

fn prompt() -> PromptEntry {
    PromptEntry {
        id: Uuid::new_v4(),
        text: "Make the sky warmer".into(),
        model: "edit-alias".into(),
        kind: MediaKind::Image,
        size: "1024x1024".into(),
        seconds: 4,
        created: Utc::now(),
        state: PromptState::Submitted,
        detail: None,
        imported: false,
        provider: Provider::LiteLlm,
        input_image: None,
    }
}
#[test]
fn sqlite_persists_many_history_entries_without_duplicates() {
    let root = tempfile::tempdir().unwrap();
    let mut book = ProjectBook::default();
    let project = book.create("Many prompts").unwrap();
    for _ in 0..500 {
        book.add_prompt(project, prompt()).unwrap();
    }
    book.save(root.path()).unwrap();
    book.save(root.path()).unwrap();
    let loaded = ProjectBook::load(root.path(), &[]).unwrap();
    assert_eq!(loaded.projects[0].prompts.len(), 500);
    let path = storage::metadata_dir(root.path()).join("studio.sqlite3");
    assert_eq!(&fs::read(&path).unwrap()[..16], b"SQLite format 3\0");
    let connection = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM prompts", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        500
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[test]
fn settings_are_restored_from_sqlite_and_have_no_api_key_field() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.sqlite3");
    let settings = Settings {
        provider: Provider::Ollama,
        ollama_url: "http://localhost:11434".into(),
        remember_key: true,
        output: root.path().join("output"),
        ..Settings::default()
    };
    Database::open(&path)
        .unwrap()
        .save_settings(&settings)
        .unwrap();
    let loaded = Database::open(&path).unwrap().settings().unwrap().unwrap();
    assert_eq!(loaded.provider, Provider::Ollama);
    assert!(loaded.remember_key);
    assert_eq!(loaded.output, settings.output);
    let data = serde_json::to_value(loaded).unwrap();
    assert!(data.get("api_key").is_none());
    assert!(data.get("key").is_none());
}
#[test]
fn old_v02_project_json_is_imported_into_sqlite_and_removed_after_success() {
    let root = tempfile::tempdir().unwrap();
    let logs = storage::metadata_dir(root.path());
    fs::create_dir_all(&logs).unwrap();
    let mut book = ProjectBook::default();
    let id = book.create("Existing project").unwrap();
    let entry = prompt();
    let entry_id = entry.id;
    book.add_prompt(id, entry).unwrap();
    let mut old = serde_json::to_value(book).unwrap();
    let prompt = &mut old["projects"][0]["prompts"][0];
    prompt.as_object_mut().unwrap().remove("provider");
    prompt.as_object_mut().unwrap().remove("input_image");
    fs::write(
        logs.join("projects.json"),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    fs::write(logs.join("gallery.json"), b"[]").unwrap();
    fs::write(logs.join("migration-v1.done"), b"1").unwrap();
    let gallery = storage::load_gallery(root.path()).unwrap();
    let loaded = ProjectBook::load(root.path(), &gallery).unwrap();
    assert_eq!(loaded.project(id).unwrap().name, "Existing project");
    assert_eq!(loaded.prompt(entry_id).unwrap().provider, Provider::LiteLlm);
    assert!(!logs.join("projects.json").exists());
    assert!(!logs.join("gallery.json").exists());
    assert!(!logs.join("migration-v1.done").exists());
    storage::migrate_legacy(root.path()).unwrap();
    assert_eq!(
        ProjectBook::load(root.path(), &[]).unwrap().projects.len(),
        1
    );
}
#[test]
fn uploaded_reference_is_preserved_with_edit_history_when_original_is_removed() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("upload.png");
    image::DynamicImage::new_rgb8(3, 2).save(&source).unwrap();
    let reference =
        storage::prepare_reference(&source, &storage::metadata_dir(root.path())).unwrap();
    fs::remove_file(source).unwrap();
    let mut book = ProjectBook::default();
    let project = book.create("Edits").unwrap();
    let mut entry = prompt();
    entry.input_image = Some(reference.clone());
    let id = entry.id;
    book.add_prompt(project, entry).unwrap();
    book.save(root.path()).unwrap();
    let loaded = ProjectBook::load(root.path(), &[]).unwrap();
    assert_eq!(
        loaded.prompt(id).unwrap().input_image,
        Some(reference.clone())
    );
    assert!(reference.is_file());
    assert_eq!(image::open(reference).unwrap().width(), 3);
}
#[test]
fn invalid_upload_does_not_create_a_reference_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("invalid.png");
    fs::write(&source, b"not an image").unwrap();
    assert!(storage::prepare_reference(&source, &storage::metadata_dir(root.path())).is_err());
    assert!(!storage::metadata_dir(root.path()).join("inputs").exists());
}
#[test]
fn credential_accounts_are_stable_and_scoped_to_provider_and_server() {
    assert_eq!(
        credentials::account(Provider::LiteLlm, "https://gateway.example/v1/"),
        credentials::account(Provider::LiteLlm, "https://gateway.example")
    );
    assert_ne!(
        credentials::account(Provider::LiteLlm, "https://gateway.example"),
        credentials::account(Provider::Ollama, "https://gateway.example")
    );
    assert_ne!(
        credentials::account(Provider::LiteLlm, "https://a.example"),
        credentials::account(Provider::LiteLlm, "https://b.example")
    );
}
#[cfg(target_os = "windows")]
#[test]
fn windows_credential_store_round_trip_across_separate_entries() {
    let url = format!("https://credential-test.invalid/{}", Uuid::new_v4());
    credentials::save(Provider::LiteLlm, &url, Some("rustlilm-test-fixture")).unwrap();
    let result = credentials::load(Provider::LiteLlm, &url);
    credentials::save(Provider::LiteLlm, &url, None).unwrap();
    assert_eq!(result.unwrap().as_deref(), Some("rustlilm-test-fixture"));
    assert!(credentials::load(Provider::LiteLlm, &url)
        .unwrap()
        .is_none());
}
