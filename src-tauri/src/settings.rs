use std::path::PathBuf;

use keyring::Entry;
use tauri::{AppHandle, Manager};

use crate::models::{
    AppError, AppResult, Provider, RepositoryProfile, SettingsBootstrap, StoredSettings,
};

const CREDENTIAL_SERVICE: &str = "com.gitpic.crossplatform";
const SETTINGS_FILENAME: &str = "settings.json";

pub async fn load_bootstrap(app: &AppHandle) -> AppResult<SettingsBootstrap> {
    let settings = load_settings(app).await?;
    let mut profiles = settings.profiles.values().cloned().collect::<Vec<_>>();
    profiles.sort_by_key(|profile| profile.provider);

    let mut token_providers = Vec::new();
    for provider in [Provider::Github, Provider::Gitlab] {
        if load_token(provider)?.is_some() {
            token_providers.push(provider);
        }
    }

    Ok(SettingsBootstrap {
        selected_provider: settings.selected_provider,
        profiles,
        token_providers,
    })
}

pub async fn load_settings(app: &AppHandle) -> AppResult<StoredSettings> {
    let path = settings_path(app)?;
    if !path.exists() {
        return Ok(StoredSettings::default());
    }

    let contents = tokio::fs::read(path).await?;
    Ok(serde_json::from_slice(&contents)?)
}

pub async fn save_profile(app: &AppHandle, profile: &RepositoryProfile) -> AppResult<()> {
    let mut settings = load_settings(app).await?;
    settings.selected_provider = profile.provider;
    settings
        .profiles
        .insert(profile.provider.key().to_owned(), profile.clone());

    let path = settings_path(app)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let data = serde_json::to_vec_pretty(&settings)?;
    let temporary_path = path.with_extension("json.tmp");
    tokio::fs::write(&temporary_path, data).await?;

    if path.exists() {
        tokio::fs::remove_file(&path).await?;
    }
    tokio::fs::rename(temporary_path, path).await?;
    Ok(())
}

pub fn load_token(provider: Provider) -> AppResult<Option<String>> {
    let entry = credential_entry(provider)?;
    match entry.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(AppError::message(format!(
            "Could not read the {} token from the operating system credential store: {error}",
            provider.display_name()
        ))),
    }
}

pub fn save_token(provider: Provider, token: &str) -> AppResult<()> {
    credential_entry(provider)?
        .set_password(token)
        .map_err(|error| {
            AppError::message(format!(
                "Could not save the {} token in the operating system credential store: {error}",
                provider.display_name()
            ))
        })
}

fn credential_entry(provider: Provider) -> AppResult<Entry> {
    Entry::new(CREDENTIAL_SERVICE, provider.key()).map_err(|error| {
        AppError::message(format!(
            "The operating system credential store is unavailable: {error}"
        ))
    })
}

fn settings_path(app: &AppHandle) -> AppResult<PathBuf> {
    app.path()
        .app_config_dir()
        .map(|directory| directory.join(SETTINGS_FILENAME))
        .map_err(|error| {
            AppError::message(format!("Could not locate the app settings folder: {error}"))
        })
}
