use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Github,
    Gitlab,
}

impl Provider {
    pub fn key(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Gitlab => "gitlab",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Github => "GitHub",
            Self::Gitlab => "GitLab",
        }
    }

    pub fn host(self) -> &'static str {
        match self {
            Self::Github => "github.com",
            Self::Gitlab => "gitlab.com",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    #[default]
    Private,
    Public,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryProfile {
    pub provider: Provider,
    pub username: String,
    pub repository_name: String,
    pub visibility: Visibility,
}

impl RepositoryProfile {
    pub fn normalized(&self) -> Self {
        Self {
            provider: self.provider,
            username: self.username.trim().to_owned(),
            repository_name: self.repository_name.trim().to_owned(),
            visibility: self.visibility,
        }
    }

    pub fn validate(&self) -> AppResult<()> {
        if self.username.trim().is_empty() {
            return Err(AppError::message(
                "Enter the username associated with the personal access token.",
            ));
        }

        let name = self.repository_name.trim();
        let valid_name = !name.is_empty()
            && name.len() <= 100
            && name
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_alphanumeric())
            && name.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
            });

        if !valid_name {
            return Err(AppError::message(
                "Repository names must start with a letter or number and contain only letters, numbers, periods, hyphens, or underscores.",
            ));
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSettings {
    pub selected_provider: Provider,
    pub profiles: BTreeMap<String, RepositoryProfile>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsBootstrap {
    pub selected_provider: Provider,
    pub profiles: Vec<RepositoryProfile>,
    pub token_providers: Vec<Provider>,
}

#[derive(Clone, Debug)]
pub struct Credentials {
    pub username: String,
    pub token: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryInfo {
    pub provider: Provider,
    pub owner: String,
    pub name: String,
    pub clone_url: String,
    pub web_url: String,
    pub default_branch: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadResult {
    pub image_count: usize,
    pub commit: String,
    pub repository_url: String,
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Message(String),
    #[error("File operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Saved settings are invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Network request failed: {0}")]
    Http(#[from] reqwest::Error),
}

impl AppError {
    pub fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_repository_names() {
        let mut profile = RepositoryProfile {
            provider: Provider::Github,
            username: "octocat".into(),
            repository_name: "photo-library_2".into(),
            visibility: Visibility::Private,
        };
        assert!(profile.validate().is_ok());

        profile.repository_name = "../outside".into();
        assert!(profile.validate().is_err());

        profile.repository_name = "-starts-with-dash".into();
        assert!(profile.validate().is_err());
    }
}
