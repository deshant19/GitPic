# GitPic — macOS and Windows

GitPic is a Tauri 2 desktop app that uploads dropped or selected images to a personal GitHub or GitLab repository. It creates the named repository when needed, reuses it afterward, and combines each queued selection into one Git commit and push.

The React interface and Rust upload engine are shared by macOS and Windows. Personal access tokens never enter command-line arguments, repository URLs, configuration files, or application logs:

- macOS stores tokens in Keychain.
- Windows stores tokens in Windows Credential Manager.
- Git receives credentials through a short-lived `GIT_ASKPASS` helper.
