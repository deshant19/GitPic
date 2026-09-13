mod git;
mod models;
mod providers;
mod settings;

use std::path::PathBuf;

use models::{
    AppError, Credentials, RepositoryInfo, RepositoryProfile, SettingsBootstrap, UploadResult,
};
use tauri::{AppHandle, Manager, State};
use tokio::sync::Mutex;

#[derive(Clone)]
struct CachedRepository {
    profile: RepositoryProfile,
    repository: RepositoryInfo,
}

struct AppState {
    client: reqwest::Client,
    upload_lock: Mutex<()>,
    repository_cache: Mutex<Option<CachedRepository>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            upload_lock: Mutex::new(()),
            repository_cache: Mutex::new(None),
        }
    }
}

#[tauri::command]
async fn load_bootstrap(app: AppHandle) -> Result<SettingsBootstrap, String> {
    settings::load_bootstrap(&app)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    profile: RepositoryProfile,
    token: Option<String>,
) -> Result<RepositoryInfo, String> {
    let profile = profile.normalized();
    profile.validate().map_err(|error| error.to_string())?;

    let supplied_token = token
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let effective_token = match supplied_token.as_ref() {
        Some(token) => token.clone(),
        None => settings::load_token(profile.provider)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                "Enter a personal access token. Account passwords cannot be used for Git uploads."
                    .to_owned()
            })?,
    };

    let repository = providers::ensure_repository(&state.client, &profile, &effective_token)
        .await
        .map_err(|error| error.to_string())?;

    if let Some(token) = supplied_token {
        settings::save_token(profile.provider, &token).map_err(|error| error.to_string())?;
    }
    settings::save_profile(&app, &profile)
        .await
        .map_err(|error| error.to_string())?;

    *state.repository_cache.lock().await = Some(CachedRepository {
        profile,
        repository: repository.clone(),
    });
    Ok(repository)
}

#[tauri::command]
async fn upload_images(
    app: AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
) -> Result<UploadResult, String> {
    let _upload_guard = state.upload_lock.lock().await;
    let stored = settings::load_settings(&app)
        .await
        .map_err(|error| error.to_string())?;
    let profile = stored
        .profiles
        .get(stored.selected_provider.key())
        .cloned()
        .ok_or_else(|| "Set up an account and repository before uploading images.".to_owned())?;
    profile.validate().map_err(|error| error.to_string())?;

    let token = settings::load_token(profile.provider)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "The saved personal access token is missing.".to_owned())?;
    let credentials = Credentials {
        username: profile.username.clone(),
        token: token.clone(),
    };

    let cached = state.repository_cache.lock().await.clone();
    let repository = if let Some(cached) = cached.filter(|cached| cached.profile == profile) {
        cached.repository
    } else {
        let repository = providers::ensure_repository(&state.client, &profile, &token)
            .await
            .map_err(|error| error.to_string())?;
        *state.repository_cache.lock().await = Some(CachedRepository {
            profile,
            repository: repository.clone(),
        });
        repository
    };

    let repositories_root = repositories_root(&app).map_err(|error| error.to_string())?;
    git::upload_images(&repositories_root, &repository, &credentials, paths)
        .await
        .map_err(|error| error.to_string())
}

fn repositories_root(app: &AppHandle) -> Result<PathBuf, AppError> {
    app.path()
        .app_data_dir()
        .map(|directory| directory.join("repositories"))
        .map_err(|error| {
            AppError::message(format!(
                "Could not locate the application data folder: {error}"
            ))
        })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            load_bootstrap,
            save_settings,
            upload_images
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
