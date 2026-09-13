use std::{
    collections::{BTreeSet, HashSet},
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
};

use tempfile::TempDir;
use tokio::process::Command;

use crate::models::{AppError, AppResult, Credentials, RepositoryInfo, UploadResult};

const IMAGE_EXTENSIONS: &[&str] = &[
    "avif", "bmp", "gif", "heic", "heif", "jpeg", "jpg", "png", "svg", "tif", "tiff", "webp",
];

pub async fn upload_images(
    repositories_root: &Path,
    repository: &RepositoryInfo,
    credentials: &Credentials,
    source_paths: Vec<String>,
) -> AppResult<UploadResult> {
    if source_paths.is_empty() {
        return Err(AppError::message("No images were selected."));
    }

    let ask_pass = AskPass::new(credentials)?;
    let prepared =
        prepare_repository(repositories_root, repository, credentials, &ask_pass).await?;
    let copied = copy_images(source_paths, &prepared.directory).await?;
    let relative_paths = copied
        .iter()
        .map(|image| image.relative_path.clone())
        .collect::<Vec<_>>();

    let mut add_arguments = vec!["add".into(), "--".into()];
    add_arguments.extend(relative_paths.iter().cloned());
    if let Err(error) = run_git_checked(&prepared.directory, add_arguments, &ask_pass).await {
        cleanup_uncommitted(&prepared.directory, &copied, &ask_pass).await;
        return Err(error);
    }

    let commit_message = if copied.len() == 1 {
        "Add image via GitPic".to_owned()
    } else {
        format!("Add {} images via GitPic", copied.len())
    };
    if let Err(error) = run_git_checked(
        &prepared.directory,
        vec!["commit".into(), "-m".into(), commit_message],
        &ask_pass,
    )
    .await
    {
        cleanup_uncommitted(&prepared.directory, &copied, &ask_pass).await;
        return Err(error);
    }

    let push_arguments = push_arguments(&prepared.branch);
    let push_output = run_git(&prepared.directory, push_arguments.clone(), &ask_pass).await?;
    if !push_output.success() {
        if is_lfs_related(&push_output.combined()) {
            recover_with_lfs(&prepared, &copied, &ask_pass).await?;
        } else {
            return Err(command_failure("git push", &push_output));
        }
    }

    let revision = run_git_checked(
        &prepared.directory,
        vec!["rev-parse".into(), "--short".into(), "HEAD".into()],
        &ask_pass,
    )
    .await?;

    Ok(UploadResult {
        image_count: copied.len(),
        commit: revision.stdout.trim().to_owned(),
        repository_url: repository.web_url.clone(),
    })
}

struct PreparedRepository {
    directory: PathBuf,
    branch: String,
}

async fn prepare_repository(
    repositories_root: &Path,
    repository: &RepositoryInfo,
    credentials: &Credentials,
    ask_pass: &AskPass,
) -> AppResult<PreparedRepository> {
    let owner = safe_path_component(&repository.owner);
    let parent = repositories_root
        .join(repository.provider.key())
        .join(owner);
    let directory = parent.join(&repository.name);
    tokio::fs::create_dir_all(&parent).await?;

    if !directory.join(".git").exists() {
        if directory.exists() {
            let mut entries = tokio::fs::read_dir(&directory).await?;
            let mut conflicts = Vec::new();
            while let Some(entry) = entries.next_entry().await? {
                if entry.file_name() == ".DS_Store" {
                    let _ = tokio::fs::remove_file(entry.path()).await;
                } else {
                    conflicts.push(entry.path());
                }
            }
            if !conflicts.is_empty() {
                return Err(AppError::message(format!(
                    "The local repository folder is not empty: {}",
                    directory.display()
                )));
            }
        }

        run_git_checked(
            &parent,
            vec![
                "clone".into(),
                "--origin".into(),
                "origin".into(),
                repository.clone_url.clone(),
                directory.to_string_lossy().into_owned(),
            ],
            ask_pass,
        )
        .await?;
    } else {
        let remote = run_git(
            &directory,
            vec!["remote".into(), "get-url".into(), "origin".into()],
            ask_pass,
        )
        .await?;
        let arguments = if remote.success() {
            vec![
                "remote".into(),
                "set-url".into(),
                "origin".into(),
                repository.clone_url.clone(),
            ]
        } else {
            vec![
                "remote".into(),
                "add".into(),
                "origin".into(),
                repository.clone_url.clone(),
            ]
        };
        run_git_checked(&directory, arguments, ask_pass).await?;
    }

    run_git_checked(
        &directory,
        vec![
            "config".into(),
            "--local".into(),
            "user.name".into(),
            credentials.username.clone(),
        ],
        ask_pass,
    )
    .await?;
    run_git_checked(
        &directory,
        vec![
            "config".into(),
            "--local".into(),
            "user.email".into(),
            format!(
                "{}@users.noreply.{}",
                credentials.username,
                repository.provider.host()
            ),
        ],
        ask_pass,
    )
    .await?;

    let branch = if repository.default_branch.trim().is_empty() {
        "main".to_owned()
    } else {
        repository.default_branch.clone()
    };
    synchronize(&directory, &branch, ask_pass).await?;

    Ok(PreparedRepository { directory, branch })
}

async fn synchronize(directory: &Path, branch: &str, ask_pass: &AskPass) -> AppResult<()> {
    run_git_checked(
        directory,
        vec!["fetch".into(), "--prune".into(), "origin".into()],
        ask_pass,
    )
    .await?;

    let local_branch = run_git(
        directory,
        vec![
            "show-ref".into(),
            "--verify".into(),
            "--quiet".into(),
            format!("refs/heads/{branch}"),
        ],
        ask_pass,
    )
    .await?
    .success();
    let remote_branch = run_git(
        directory,
        vec![
            "show-ref".into(),
            "--verify".into(),
            "--quiet".into(),
            format!("refs/remotes/origin/{branch}"),
        ],
        ask_pass,
    )
    .await?
    .success();

    let checkout = if local_branch {
        vec!["checkout".into(), branch.into()]
    } else if remote_branch {
        vec![
            "checkout".into(),
            "-B".into(),
            branch.into(),
            format!("origin/{branch}"),
        ]
    } else {
        vec!["checkout".into(), "-B".into(), branch.into()]
    };
    run_git_checked(directory, checkout, ask_pass).await?;

    if remote_branch {
        run_git_checked(
            directory,
            vec![
                "pull".into(),
                "--rebase".into(),
                "origin".into(),
                branch.into(),
            ],
            ask_pass,
        )
        .await?;
    }

    Ok(())
}

#[derive(Debug)]
struct CopiedImage {
    destination: PathBuf,
    relative_path: String,
    extension: String,
}

async fn copy_images(source_paths: Vec<String>, repository: &Path) -> AppResult<Vec<CopiedImage>> {
    let mut copied = Vec::new();
    let mut seen = HashSet::new();

    for raw_path in source_paths {
        let source = PathBuf::from(raw_path);
        let canonical = match tokio::fs::canonicalize(&source).await {
            Ok(path) => path,
            Err(_) => continue,
        };
        if !seen.insert(canonical.clone()) || !canonical.is_file() {
            continue;
        }

        let extension = canonical
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !IMAGE_EXTENSIONS.contains(&extension.as_str()) {
            continue;
        }

        let Some(filename) = canonical.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let destination = unique_destination(repository, filename);
        if let Err(error) = tokio::fs::copy(&canonical, &destination).await {
            remove_copied_files(&copied).await;
            return Err(error.into());
        }
        copied.push(CopiedImage {
            relative_path: destination
                .file_name()
                .expect("destination always has a filename")
                .to_string_lossy()
                .into_owned(),
            destination,
            extension,
        });
    }

    if copied.is_empty() {
        return Err(AppError::message(
            "The selected items do not contain any supported image files.",
        ));
    }

    Ok(copied)
}

fn unique_destination(directory: &Path, filename: &str) -> PathBuf {
    let original = Path::new(filename);
    let stem = original
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("image");
    let extension = original.extension().and_then(|value| value.to_str());

    let mut candidate = directory.join(filename);
    let mut suffix = 1;
    while candidate.exists() {
        let name = match extension {
            Some(extension) => format!("{stem}-{suffix}.{extension}"),
            None => format!("{stem}-{suffix}"),
        };
        candidate = directory.join(name);
        suffix += 1;
    }
    candidate
}

async fn cleanup_uncommitted(directory: &Path, copied: &[CopiedImage], ask_pass: &AskPass) {
    let mut reset = vec!["reset".into(), "--".into()];
    reset.extend(copied.iter().map(|image| image.relative_path.clone()));
    let _ = run_git(directory, reset, ask_pass).await;
    remove_copied_files(copied).await;
}

async fn remove_copied_files(copied: &[CopiedImage]) {
    for image in copied {
        let _ = tokio::fs::remove_file(&image.destination).await;
    }
}

async fn recover_with_lfs(
    prepared: &PreparedRepository,
    images: &[CopiedImage],
    ask_pass: &AskPass,
) -> AppResult<()> {
    let version = run_git(
        &prepared.directory,
        vec!["lfs".into(), "version".into()],
        ask_pass,
    )
    .await?;
    if !version.success() {
        return Err(AppError::message(
            "Git LFS is required for this upload but is not installed. Install Git LFS, then try again.",
        ));
    }

    let result = async {
        run_git_checked(
            &prepared.directory,
            vec!["lfs".into(), "install".into(), "--local".into()],
            ask_pass,
        )
        .await?;

        let patterns = images
            .iter()
            .map(|image| {
                if image.extension.is_empty() {
                    image.relative_path.clone()
                } else {
                    format!("*.{}", image.extension)
                }
            })
            .collect::<BTreeSet<_>>();
        let mut track = vec!["lfs".into(), "track".into()];
        track.extend(patterns);
        run_git_checked(&prepared.directory, track, ask_pass).await?;

        let mut add = vec!["add".into(), "--".into(), ".gitattributes".into()];
        add.extend(images.iter().map(|image| image.relative_path.clone()));
        run_git_checked(&prepared.directory, add, ask_pass).await?;
        run_git_checked(
            &prepared.directory,
            vec!["commit".into(), "--amend".into(), "--no-edit".into()],
            ask_pass,
        )
        .await?;
        run_git_checked(
            &prepared.directory,
            push_arguments(&prepared.branch),
            ask_pass,
        )
        .await?;
        AppResult::Ok(())
    }
    .await;

    result.map_err(|error| AppError::message(format!("Git LFS recovery did not complete: {error}")))
}

fn push_arguments(branch: &str) -> Vec<String> {
    vec![
        "push".into(),
        "origin".into(),
        format!("HEAD:refs/heads/{branch}"),
    ]
}

struct CommandOutput {
    status: i32,
    stdout: String,
    stderr: String,
}

impl CommandOutput {
    fn success(&self) -> bool {
        self.status == 0
    }

    fn combined(&self) -> String {
        [self.stderr.as_str(), self.stdout.as_str()]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

async fn run_git_checked(
    directory: &Path,
    arguments: Vec<String>,
    ask_pass: &AskPass,
) -> AppResult<CommandOutput> {
    let description = format!("git {}", arguments.join(" "));
    let output = run_git(directory, arguments, ask_pass).await?;
    if output.success() {
        Ok(output)
    } else {
        Err(command_failure(&description, &output))
    }
}

async fn run_git(
    directory: &Path,
    arguments: Vec<String>,
    ask_pass: &AskPass,
) -> AppResult<CommandOutput> {
    let executable = git_executable();
    let mut command = Command::new(&executable);
    command
        .args(&arguments)
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_ASKPASS", &ask_pass.script_path)
        .env("GIT_ASKPASS_REQUIRE", "force")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GITPIC_USERNAME", &ask_pass.username)
        .env("GITPIC_TOKEN", &ask_pass.token)
        .env("PATH", git_path_environment(&executable));

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.as_std_mut().creation_flags(CREATE_NO_WINDOW);
    }

    let output = command.output().await.map_err(|error| {
        AppError::message(format!(
            "Could not launch Git at {}: {error}",
            executable.display()
        ))
    })?;

    Ok(CommandOutput {
        status: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn command_failure(command: &str, output: &CommandOutput) -> AppError {
    let detail = output.combined().trim().to_owned();
    if detail.is_empty() {
        AppError::message(format!("{command} failed with status {}.", output.status))
    } else {
        AppError::message(detail)
    }
}

fn is_lfs_related(output: &str) -> bool {
    const MARKERS: &[&str] = &[
        "git lfs",
        "git-lfs",
        "large files detected",
        "file size limit",
        "exceeds github's file size limit",
        "exceeds gitlab's maximum allowed size",
        "file is larger than the allowed size",
        "lfs is not enabled",
        "lfs objects are missing",
        "batch response:",
        "gh001:",
    ];

    let output = output.to_ascii_lowercase();
    MARKERS.iter().any(|marker| output.contains(marker))
}

fn safe_path_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn git_executable() -> PathBuf {
    if let Some(path) = env::var_os("GITPIC_GIT_PATH") {
        return PathBuf::from(path);
    }
    platform_git_executable()
}

#[cfg(target_os = "macos")]
fn platform_git_executable() -> PathBuf {
    PathBuf::from("/usr/bin/git")
}

#[cfg(target_os = "windows")]
fn platform_git_executable() -> PathBuf {
    for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(root) = env::var_os(variable) {
            let candidate = PathBuf::from(root).join("Git").join("cmd").join("git.exe");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("git.exe")
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_git_executable() -> PathBuf {
    PathBuf::from("git")
}

fn git_path_environment(executable: &Path) -> OsString {
    let mut paths = Vec::new();
    if let Some(parent) = executable
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        paths.push(parent.to_path_buf());
    }

    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/opt/homebrew/bin"));
        paths.push(PathBuf::from("/usr/local/bin"));
        paths.push(PathBuf::from("/usr/bin"));
        paths.push(PathBuf::from("/bin"));
    }

    if let Some(existing) = env::var_os("PATH") {
        paths.extend(env::split_paths(&existing));
    }
    env::join_paths(paths).unwrap_or_else(|_| env::var_os("PATH").unwrap_or_default())
}

struct AskPass {
    _directory: TempDir,
    script_path: PathBuf,
    username: String,
    token: String,
}

impl AskPass {
    fn new(credentials: &Credentials) -> AppResult<Self> {
        let directory = tempfile::Builder::new()
            .prefix("GitPic-AskPass-")
            .tempdir()?;

        #[cfg(target_os = "windows")]
        let (script_path, script) = (
            directory.path().join("askpass.cmd"),
            "@echo off\r\necho %~1 | findstr /I \"Username\" >nul\r\nif %errorlevel%==0 (echo %GITPIC_USERNAME%) else (echo %GITPIC_TOKEN%)\r\n",
        );

        #[cfg(not(target_os = "windows"))]
        let (script_path, script) = (
            directory.path().join("askpass.sh"),
            "#!/bin/sh\ncase \"$1\" in\n  *sername*) printf '%s\\n' \"$GITPIC_USERNAME\" ;;\n  *) printf '%s\\n' \"$GITPIC_TOKEN\" ;;\nesac\n",
        );

        std::fs::write(&script_path, script)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700))?;
        }

        Ok(Self {
            _directory: directory,
            script_path,
            username: credentials.username.clone(),
            token: credentials.token.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Provider;

    #[test]
    fn recognizes_lfs_failures_without_matching_auth_errors() {
        assert!(is_lfs_related(
            "remote: error: GH001: Large files detected."
        ));
        assert!(is_lfs_related(
            "batch response: LFS is not enabled for this project"
        ));
        assert!(!is_lfs_related("fatal: Authentication failed"));
    }

    #[test]
    fn creates_collision_safe_destinations() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("photo.png"), b"first").unwrap();
        assert_eq!(
            unique_destination(directory.path(), "photo.png")
                .file_name()
                .unwrap(),
            "photo-1.png"
        );
    }

    #[tokio::test]
    async fn commits_and_pushes_a_real_local_batch() {
        let root = tempfile::tempdir().unwrap();
        let bare_repository = root.path().join("remote.git");
        let initialization = std::process::Command::new(git_executable())
            .args(["init", "--bare"])
            .arg(&bare_repository)
            .output()
            .expect("Git must be installed to run this integration test");
        assert!(
            initialization.status.success(),
            "{}",
            String::from_utf8_lossy(&initialization.stderr)
        );

        let first = root.path().join("one.png");
        let second = root.path().join("two.jpg");
        std::fs::write(&first, b"png").unwrap();
        std::fs::write(&second, b"jpeg").unwrap();

        let repository = RepositoryInfo {
            provider: Provider::Github,
            owner: "local-user".into(),
            name: "pictures".into(),
            clone_url: bare_repository.to_string_lossy().into_owned(),
            web_url: "https://example.invalid/local-user/pictures".into(),
            default_branch: "main".into(),
        };
        let credentials = Credentials {
            username: "local-user".into(),
            token: "unused".into(),
        };

        let result = upload_images(
            &root.path().join("repositories"),
            &repository,
            &credentials,
            vec![
                first.to_string_lossy().into_owned(),
                second.to_string_lossy().into_owned(),
            ],
        )
        .await
        .unwrap();
        assert_eq!(result.image_count, 2);

        let log = std::process::Command::new(git_executable())
            .arg("--git-dir")
            .arg(&bare_repository)
            .args(["log", "-1", "--pretty=%s", "main"])
            .output()
            .unwrap();
        assert!(log.status.success());
        assert_eq!(
            String::from_utf8_lossy(&log.stdout).trim(),
            "Add 2 images via GitPic"
        );
    }
}
