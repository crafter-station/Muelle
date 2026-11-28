//! Read-only OAuth inputs. Never refresh, rewrite, or search other accounts.
use super::{Provider, State};
use serde_json::Value;
use std::{
    fs::File,
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
};

const MAX_JSON_BYTES: u64 = 1024 * 1024;

pub(super) fn claude_path(config: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    let root = match config.filter(|path| !path.as_os_str().is_empty()) {
        Some(path) => path.to_path_buf(),
        None => home?.join(".claude"),
    };
    root.is_absolute().then(|| root.join(".credentials.json"))
}

pub(super) fn gemini_path(config_home: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    // GEMINI_CLI_HOME replaces the home, not the .gemini directory itself.
    let root = config_home
        .filter(|path| !path.as_os_str().is_empty())
        .or(home)?;
    root.is_absolute()
        .then(|| root.join(".gemini/oauth_creds.json"))
}

pub(super) fn read_credentials(path: &Path, provider: Provider) -> Result<Value, State> {
    let file = File::open(path).map_err(|error| {
        if error.kind() == ErrorKind::NotFound {
            State::SignIn
        } else {
            muelle_shell::log::line(&format!(
                "uso IA {}: lectura de sesión no disponible ({:?})",
                provider.name(),
                error.kind()
            ));
            State::Unavailable
        }
    })?;
    if file.metadata().map_err(|_| State::Unavailable)?.len() > MAX_JSON_BYTES {
        return Err(State::Unavailable);
    }
    // Bound reads even if the client replaces the file while we read it.
    serde_json::from_reader(file.take(MAX_JSON_BYTES)).map_err(|_| {
        muelle_shell::log::line(&format!(
            "uso IA {}: formato de sesión no disponible",
            provider.name()
        ));
        State::Unavailable
    })
}

pub(super) fn response_json(
    response: reqwest::blocking::Response,
    provider: Provider,
) -> Result<Value, State> {
    // A forbidden quota is not proof of logout: the session may lack a scope,
    // subscription permission, or access through a corporate proxy.
    let status = response.status();
    if !status.is_success() {
        // Never record tokens, response bodies, account names or paths.
        muelle_shell::log::line(&format!(
            "uso IA {}: consulta de cuota HTTP {}",
            provider.name(),
            status.as_u16()
        ));
    }
    if status.as_u16() == 401 {
        return Err(State::SignIn);
    }
    if !status.is_success() {
        return Err(State::Unavailable);
    }
    serde_json::from_reader(response.take(MAX_JSON_BYTES)).map_err(|_| State::Unavailable)
}
