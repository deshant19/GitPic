use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::{Client, Response, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::models::{AppError, AppResult, Provider, RepositoryInfo, RepositoryProfile, Visibility};

const USER_AGENT: &str = "GitPic/1.0";

pub async fn ensure_repository(
    client: &Client,
    profile: &RepositoryProfile,
    token: &str,
) -> AppResult<RepositoryInfo> {
    match profile.provider {
        Provider::Github => github_repository(client, profile, token).await,
        Provider::Gitlab => gitlab_repository(client, profile, token).await,
    }
}

async fn github_repository(
    client: &Client,
    profile: &RepositoryProfile,
    token: &str,
) -> AppResult<RepositoryInfo> {
    let user_response = client
        .get("https://api.github.com/user")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", USER_AGENT)
        .bearer_auth(token)
        .send()
        .await?;
    let user: GitHubUser = decode_response(Provider::Github, user_response).await?;

    if !user.login.eq_ignore_ascii_case(&profile.username) {
        return Err(AppError::message(format!(
            "The GitHub token belongs to “{}”, not “{}”.",
            user.login, profile.username
        )));
    }

    let repository_url = format!(
        "https://api.github.com/repos/{}/{}",
        user.login, profile.repository_name
    );
    let lookup = client
        .get(repository_url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", USER_AGENT)
        .bearer_auth(token)
        .send()
        .await?;

    let repository: GitHubRepository = if lookup.status() == StatusCode::NOT_FOUND {
        let create = client
            .post("https://api.github.com/user/repos")
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", USER_AGENT)
            .bearer_auth(token)
            .json(&GitHubCreateRepository {
                name: &profile.repository_name,
                private: profile.visibility == Visibility::Private,
                auto_init: false,
            })
            .send()
            .await?;
        decode_response(Provider::Github, create).await?
    } else {
        decode_response(Provider::Github, lookup).await?
    };

    Ok(RepositoryInfo {
        provider: Provider::Github,
        owner: repository.owner.login,
        name: repository.name,
        clone_url: repository.clone_url,
        web_url: repository.html_url,
        default_branch: repository.default_branch.unwrap_or_else(|| "main".into()),
    })
}

async fn gitlab_repository(
    client: &Client,
    profile: &RepositoryProfile,
    token: &str,
) -> AppResult<RepositoryInfo> {
    let user_response = client
        .get("https://gitlab.com/api/v4/user")
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT)
        .header("PRIVATE-TOKEN", token)
        .send()
        .await?;
    let user: GitLabUser = decode_response(Provider::Gitlab, user_response).await?;

    if !user.username.eq_ignore_ascii_case(&profile.username) {
        return Err(AppError::message(format!(
            "The GitLab token belongs to “{}”, not “{}”.",
            user.username, profile.username
        )));
    }

    let project_path = format!("{}/{}", user.username, profile.repository_name);
    let encoded_path = utf8_percent_encode(&project_path, NON_ALPHANUMERIC).to_string();
    let lookup = client
        .get(format!("https://gitlab.com/api/v4/projects/{encoded_path}"))
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT)
        .header("PRIVATE-TOKEN", token)
        .send()
        .await?;

    let project: GitLabProject = if lookup.status() == StatusCode::NOT_FOUND {
        let create = client
            .post("https://gitlab.com/api/v4/projects")
            .header("Accept", "application/json")
            .header("User-Agent", USER_AGENT)
            .header("PRIVATE-TOKEN", token)
            .json(&GitLabCreateProject {
                name: &profile.repository_name,
                path: &profile.repository_name,
                visibility: match profile.visibility {
                    Visibility::Private => "private",
                    Visibility::Public => "public",
                },
                lfs_enabled: true,
            })
            .send()
            .await?;
        decode_response(Provider::Gitlab, create).await?
    } else {
        decode_response(Provider::Gitlab, lookup).await?
    };

    let owner = project
        .path_with_namespace
        .rsplit_once('/')
        .map(|(owner, _)| owner)
        .unwrap_or(&user.username)
        .to_owned();

    Ok(RepositoryInfo {
        provider: Provider::Gitlab,
        owner,
        name: project.path,
        clone_url: project.http_url_to_repo,
        web_url: project.web_url,
        default_branch: project.default_branch.unwrap_or_else(|| "main".into()),
    })
}

async fn decode_response<T: DeserializeOwned>(
    provider: Provider,
    response: Response,
) -> AppResult<T> {
    let status = response.status();
    let body = response.text().await?;

    if !status.is_success() {
        let fallback = status
            .canonical_reason()
            .unwrap_or("The provider rejected the request.");
        let message = provider_error_message(&body).unwrap_or_else(|| fallback.to_owned());
        let prefix = if matches!(status.as_u16(), 401 | 403) {
            format!(
                "{} rejected the credentials or token permissions",
                provider.display_name()
            )
        } else {
            format!("{} request failed", provider.display_name())
        };
        return Err(AppError::message(format!("{prefix}: {message}")));
    }

    serde_json::from_str(&body).map_err(|_| {
        AppError::message(format!(
            "{} returned a response GitPic could not understand.",
            provider.display_name()
        ))
    })
}

fn provider_error_message(body: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    if let Some(message) = value.get("message").and_then(|value| value.as_str()) {
        return Some(message.to_owned());
    }
    if let Some(error) = value.get("error").and_then(|value| value.as_str()) {
        return Some(error.to_owned());
    }
    value.get("message").map(|message| message.to_string())
}

#[derive(Deserialize)]
struct GitHubUser {
    login: String,
}

#[derive(Deserialize)]
struct GitHubRepository {
    name: String,
    clone_url: String,
    html_url: String,
    default_branch: Option<String>,
    owner: GitHubOwner,
}

#[derive(Deserialize)]
struct GitHubOwner {
    login: String,
}

#[derive(Serialize)]
struct GitHubCreateRepository<'a> {
    name: &'a str,
    private: bool,
    auto_init: bool,
}

#[derive(Deserialize)]
struct GitLabUser {
    username: String,
}

#[derive(Deserialize)]
struct GitLabProject {
    path: String,
    path_with_namespace: String,
    http_url_to_repo: String,
    web_url: String,
    default_branch: Option<String>,
}

#[derive(Serialize)]
struct GitLabCreateProject<'a> {
    name: &'a str,
    path: &'a str,
    visibility: &'a str,
    lfs_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_provider_messages() {
        assert_eq!(
            provider_error_message(r#"{"message":"Bad credentials"}"#).as_deref(),
            Some("Bad credentials")
        );
        assert_eq!(
            provider_error_message(r#"{"error":"invalid_token"}"#).as_deref(),
            Some("invalid_token")
        );
    }
}
