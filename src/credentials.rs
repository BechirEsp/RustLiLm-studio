use crate::storage::Provider;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

pub fn account(provider: Provider, url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let url = match provider {
        Provider::LiteLlm => url.strip_suffix("/v1").unwrap_or(url),
        Provider::Ollama => url.strip_suffix("/api").unwrap_or(url),
    };
    format!("{}:{:x}", provider.label(), Sha256::digest(url.as_bytes()))
}
fn entry(provider: Provider, url: &str) -> Result<keyring::Entry> {
    keyring::Entry::new("RustLiLm Studio", &account(provider, url))
        .context("Cannot open the operating system credential store")
}
pub fn load(provider: Provider, url: &str) -> Result<Option<String>> {
    match entry(provider,url)?.get_password() {
        Ok(key) => Ok(Some(key)), Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error).context("Cannot read the saved API key. Unlock your OS credential store or use the API-key field/environment variable"),
    }
}
pub fn save(provider: Provider, url: &str, key: Option<&str>) -> Result<()> {
    let entry = entry(provider, url)?;
    if let Some(key) = key.filter(|s| !s.is_empty()) {
        entry
            .set_password(key)
            .context("The API key could not be saved in the OS credential store")?;
    } else {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(error) => return Err(error).context("Cannot remove the saved API key"),
        }
    }
    Ok(())
}
