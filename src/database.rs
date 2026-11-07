use crate::{
    gateway::PendingVideo,
    projects::{Project, ProjectBook, PromptEntry},
    storage::{Asset, Settings},
};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::{fs, path::Path, time::Duration};

pub struct Database {
    connection: Connection,
}
impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        fs::create_dir_all(path.parent().context("Database path has no parent")?)?;
        let connection = Connection::open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        connection.busy_timeout(Duration::from_secs(10))?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS settings (name TEXT PRIMARY KEY, data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS metadata (name TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, position INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS prompts (id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
                position INTEGER NOT NULL, created TEXT NOT NULL, state TEXT NOT NULL, data TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS prompts_by_project ON prompts(project_id, position);
            CREATE TABLE IF NOT EXISTS assets (id TEXT PRIMARY KEY, project TEXT NOT NULL, prompt_id TEXT,
                path TEXT NOT NULL, created TEXT NOT NULL, data TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS assets_by_project ON assets(project, created DESC);
            CREATE INDEX IF NOT EXISTS assets_by_prompt ON assets(prompt_id);
            CREATE TABLE IF NOT EXISTS video_jobs (id TEXT PRIMARY KEY, project TEXT NOT NULL, model TEXT NOT NULL, data TEXT NOT NULL);
            PRAGMA user_version=1;")?;
        Ok(Self { connection })
    }
    pub fn workspace(logs: &Path) -> Result<Self> {
        Self::open(&logs.join("studio.sqlite3"))
    }
    pub fn settings(&self) -> Result<Option<Settings>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM settings WHERE name='connection'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        data.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }
    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        self.connection.execute(
            "INSERT INTO settings(name,data) VALUES('connection',?1)
            ON CONFLICT(name) DO UPDATE SET data=excluded.data",
            [serde_json::to_string(settings)?],
        )?;
        Ok(())
    }
    pub fn migrated(&self) -> Result<bool> {
        Ok(self
            .connection
            .query_row(
                "SELECT value FROM metadata WHERE name='json-migration'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .is_some())
    }
    pub fn finish_migration(&self) -> Result<()> {
        self.connection.execute(
            "INSERT OR REPLACE INTO metadata(name,value) VALUES('json-migration','1')",
            [],
        )?;
        Ok(())
    }
    pub fn save_assets(&mut self, assets: &[Asset]) -> Result<()> {
        let tx = self.connection.transaction()?;
        for asset in assets {
            tx.execute("INSERT INTO assets(id,project,prompt_id,path,created,data) VALUES(?1,?2,?3,?4,?5,?6)
                ON CONFLICT(id) DO UPDATE SET data=excluded.data,path=excluded.path,prompt_id=excluded.prompt_id WHERE assets.data<>excluded.data",
                params![asset.id.to_string(), asset.project, asset.prompt_id.map(|id| id.to_string()), asset.path.to_string_lossy(), asset.created.to_rfc3339(), serde_json::to_string(asset)?])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn assets(&self) -> Result<Vec<Asset>> {
        let mut query = self
            .connection
            .prepare("SELECT data FROM assets ORDER BY created DESC, id DESC")?;
        let rows = query.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn save_projects(&mut self, book: &ProjectBook) -> Result<()> {
        let tx = self.connection.transaction()?;
        for (position, project) in book.projects.iter().enumerate() {
            tx.execute("INSERT INTO projects(id,name,position) VALUES(?1,?2,?3)
                ON CONFLICT(id) DO UPDATE SET name=excluded.name,position=excluded.position WHERE projects.name<>excluded.name OR projects.position<>excluded.position",
                params![project.id.to_string(),project.name,position as i64])?;
            for (index, prompt) in project.prompts.iter().enumerate() {
                tx.execute("INSERT INTO prompts(id,project_id,position,created,state,data) VALUES(?1,?2,?3,?4,?5,?6)
                    ON CONFLICT(id) DO UPDATE SET position=excluded.position,state=excluded.state,data=excluded.data
                    WHERE prompts.data<>excluded.data OR prompts.position<>excluded.position",
                    params![prompt.id.to_string(),project.id.to_string(),index as i64,prompt.created.to_rfc3339(),format!("{:?}",prompt.state),serde_json::to_string(prompt)?])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub fn projects(&self) -> Result<ProjectBook> {
        let mut book = ProjectBook::default();
        let mut query = self
            .connection
            .prepare("SELECT id,name FROM projects ORDER BY position")?;
        for row in query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (id, name) = row?;
            let mut prompts = self
                .connection
                .prepare("SELECT data FROM prompts WHERE project_id=?1 ORDER BY position")?;
            let entries: Vec<PromptEntry> = prompts
                .query_map([&id], |r| r.get::<_, String>(0))?
                .map(|r| Ok(serde_json::from_str(&r?)?))
                .collect::<Result<_>>()?;
            book.projects.push(Project {
                id: id.parse()?,
                name,
                prompts: entries,
            });
        }
        Ok(book)
    }
    pub fn save_job(&self, pending: &PendingVideo) -> Result<()> {
        self.connection.execute(
            "INSERT INTO video_jobs(id,project,model,data) VALUES(?1,?2,?3,?4)
            ON CONFLICT(id) DO UPDATE SET data=excluded.data",
            params![
                pending.id,
                pending.request.project,
                pending.request.model,
                serde_json::to_string(pending)?
            ],
        )?;
        Ok(())
    }
    pub fn jobs(&self) -> Result<Vec<PendingVideo>> {
        let mut query = self
            .connection
            .prepare("SELECT data FROM video_jobs ORDER BY rowid DESC")?;
        let rows = query.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn remove_job(&self, id: &str) -> Result<()> {
        self.connection
            .execute("DELETE FROM video_jobs WHERE id=?1", [id])?;
        Ok(())
    }
}
