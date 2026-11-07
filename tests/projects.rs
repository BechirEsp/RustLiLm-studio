use chrono::Utc;
use rustlilm_studio::{
    projects::{ProjectBook, PromptEntry, PromptState},
    storage::{self, Asset, AssetDetails, MediaKind},
};
use std::{fs, path::Path};
use uuid::Uuid;

fn entry(text: &str) -> PromptEntry {
    PromptEntry {
        id: Uuid::new_v4(),
        text: text.into(),
        model: "image-alias".into(),
        kind: MediaKind::Image,
        size: "1536x1024".into(),
        seconds: 4,
        created: Utc::now(),
        state: PromptState::Submitted,
        detail: None,
        imported: false,
    }
}

fn legacy_asset(root: &Path) -> Asset {
    let path = root.join("My_project").join("2026-11-06").join("image.png");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"image fixture").unwrap();
    Asset {
        id: Uuid::new_v4(),
        path,
        kind: MediaKind::Image,
        model: "old-image-alias".into(),
        prompt: "Old prompt".into(),
        project: "My project".into(),
        created: Utc::now(),
        prompt_id: None,
    }
}

#[test]
fn projects_and_prompt_history_survive_restart_without_outputs() {
    let root = tempfile::tempdir().unwrap();
    let mut book = ProjectBook::default();
    let first = book.create("Project one").unwrap();
    let second = book.create("Project two").unwrap();
    let prompt = entry("a lighthouse");
    let prompt_id = prompt.id;
    book.add_prompt(first, prompt).unwrap();
    book.finish(prompt_id, Err("Gateway unavailable".into()));
    book.save(root.path()).unwrap();
    let loaded = ProjectBook::load(root.path(), &[]).unwrap();
    assert_eq!(loaded.projects.len(), 2);
    assert!(loaded.project(second).unwrap().prompts.is_empty());
    let prompt = loaded.prompt(prompt_id).unwrap();
    assert_eq!(prompt.text, "a lighthouse");
    assert_eq!(prompt.size, "1536x1024");
    assert_eq!(prompt.state, PromptState::Failed);
    assert_eq!(prompt.detail.as_deref(), Some("Gateway unavailable"));
    assert!(storage::metadata_dir(root.path())
        .join("projects.json")
        .exists());
}

#[test]
fn project_names_cannot_share_an_output_folder() {
    let mut book = ProjectBook::default();
    book.create("My project").unwrap();
    assert!(book.create("my PROJECT").is_err());
    assert!(book.create("My_project").is_err());
    assert!(book.create("   ").is_err());
    assert!(book.create(&"a".repeat(73)).is_err());
    assert!(book.create("Another project").is_ok());
}

#[test]
fn repeated_prompts_have_distinct_history_entries_and_results() {
    let mut book = ProjectBook::default();
    let project = book.create("Example").unwrap();
    let first = entry("same prompt");
    let second = entry("same prompt");
    let (a, b) = (first.id, second.id);
    book.add_prompt(project, first).unwrap();
    book.add_prompt(project, second).unwrap();
    book.finish(a, Ok(()));
    assert_eq!(book.project(project).unwrap().prompts.len(), 2);
    assert_eq!(book.prompt(a).unwrap().state, PromptState::Completed);
    assert_eq!(book.prompt(b).unwrap().state, PromptState::Submitted);
}

#[test]
fn legacy_gallery_and_sidecars_move_without_moving_images_or_unrelated_json() {
    let root = tempfile::tempdir().unwrap();
    let asset = legacy_asset(root.path());
    let mut value = serde_json::to_value(&asset).unwrap();
    value.as_object_mut().unwrap().remove("prompt_id");
    fs::write(
        asset.path.with_extension("json"),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    fs::write(
        root.path().join(".rustlilm-gallery.json"),
        serde_json::to_vec(&vec![value]).unwrap(),
    )
    .unwrap();
    let unrelated = asset.path.parent().unwrap().join("my-notes.json");
    fs::write(&unrelated, b"{\"my\":\"notes\"}").unwrap();
    let gallery = storage::load_gallery(root.path()).unwrap();
    assert_eq!(gallery[0].id, asset.id);
    assert!(asset.path.exists());
    assert!(unrelated.exists());
    assert!(!asset.path.with_extension("json").exists());
    assert!(!root.path().join(".rustlilm-gallery.json").exists());
    assert!(storage::metadata_dir(root.path())
        .join("assets")
        .join(format!("{}.json", asset.id))
        .exists());
    assert_eq!(storage::load_gallery(root.path()).unwrap().len(), 1);
    let book = ProjectBook::load(root.path(), &gallery).unwrap();
    assert_eq!(book.projects[0].name, "My project");
    assert_eq!(book.projects[0].prompts[0].text, "Old prompt");
    let reloaded = ProjectBook::load(root.path(), &gallery).unwrap();
    assert_eq!(reloaded.projects.len(), 1);
    assert_eq!(reloaded.projects[0].prompts.len(), 1);
}

#[test]
fn legacy_video_checkpoint_moves_to_shared_jobs_folder() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("Project").join("2026-11-06");
    fs::create_dir_all(&folder).unwrap();
    let old = folder.join("pending-video.json");
    let request = serde_json::json!({"id":"opaque/video-id","request":{"model":"video-alias","prompt":"a scene","project":"Project",
        "kind":"Video","size":"1280x720","seconds":4,"destination":folder}});
    fs::write(&old, serde_json::to_vec(&request).unwrap()).unwrap();
    storage::migrate_legacy(root.path()).unwrap();
    assert!(!old.exists());
    let jobs = storage::metadata_dir(root.path()).join("jobs");
    let path = fs::read_dir(&jobs).unwrap().next().unwrap().unwrap().path();
    let pending: rustlilm_studio::gateway::PendingVideo =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(pending.checkpoint, path);
    assert_eq!(pending.request.destination, folder);
    assert_eq!(pending.request.logs, storage::metadata_dir(root.path()));
    assert_eq!(pending.id, "opaque/video-id");
    storage::migrate_legacy(root.path()).unwrap();
    assert_eq!(fs::read_dir(jobs).unwrap().count(), 1);
}

#[test]
fn outputs_keep_metadata_outside_project_folders_and_recover_completed_history() {
    let root = tempfile::tempdir().unwrap();
    let mut book = ProjectBook::default();
    let project = book.create("Project").unwrap();
    let prompt = entry("a scene");
    let prompt_id = prompt.id;
    book.add_prompt(project, prompt).unwrap();
    book.save(root.path()).unwrap();
    let folder = root.path().join("Project").join("2026-11-07");
    let asset = storage::save_asset(
        &folder,
        &storage::metadata_dir(root.path()),
        b"fixture",
        "png",
        AssetDetails {
            kind: MediaKind::Image,
            model: "image-alias",
            prompt: "a scene",
            project: "Project",
            prompt_id: Some(prompt_id),
        },
    )
    .unwrap();
    assert_eq!(fs::read_dir(folder).unwrap().count(), 1);
    let loaded = ProjectBook::load(root.path(), &[asset]).unwrap();
    assert_eq!(
        loaded.prompt(prompt_id).unwrap().state,
        PromptState::Completed
    );
    assert_eq!(loaded.project(project).unwrap().prompts.len(), 1);
}

#[test]
fn rerunning_a_legacy_prompt_does_not_show_the_old_output_as_a_new_result() {
    let root = tempfile::tempdir().unwrap();
    let legacy = legacy_asset(root.path());
    let mut book = ProjectBook::load(root.path(), std::slice::from_ref(&legacy)).unwrap();
    let project = book.projects[0].id;
    let imported = book.projects[0].prompts[0].id;
    let mut retry = entry(&legacy.prompt);
    retry.model = legacy.model.clone();
    let retry_id = retry.id;
    book.add_prompt(project, retry).unwrap();
    assert!(book.matches_prompt(&legacy, imported));
    assert!(!book.matches_prompt(&legacy, retry_id));
    let new_asset = Asset {
        id: Uuid::new_v4(),
        prompt_id: Some(retry_id),
        ..legacy.clone()
    };
    assert!(book.matches_prompt(&new_asset, retry_id));
    assert!(!book.matches_prompt(&new_asset, imported));
}
