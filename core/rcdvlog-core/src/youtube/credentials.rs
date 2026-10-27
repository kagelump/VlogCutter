#[cfg(target_os = "macos")]
use super::failure;
use super::{UPLOAD_SCOPE, exact_scope, now};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

// Deliberately no Debug or Serialize on the public status's source credential.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Credentials {
    pub id: String,
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
    pub access_token: String,
    pub expires_at: u64,
    pub scope: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub authorized: bool,
    pub credential_id: Option<String>,
    pub scope: &'static str,
    pub storage: &'static str,
    pub access_token_expired: bool,
}

/// Explicit files work on headless hosts. macOS defaults to Keychain.
pub struct CredentialStore {
    path: PathBuf,
    keychain: bool,
}

impl CredentialStore {
    pub fn new(file: Option<&Path>) -> Result<Self> {
        let path = match file {
            Some(path) => {
                ensure!(
                    path.is_absolute(),
                    "invalid input: credential file must be absolute"
                );
                path.to_owned()
            }
            None => dirs::data_local_dir()
                .context("locate local application data directory")?
                .join("com.rcd.vlogcutter/youtube/credentials.json"),
        };
        Ok(Self {
            path,
            keychain: cfg!(target_os = "macos") && file.is_none(),
        })
    }

    pub fn status(&self) -> Result<AuthStatus> {
        let credentials = self.load_optional()?;
        Ok(AuthStatus {
            authorized: credentials.is_some(),
            credential_id: credentials.as_ref().map(|c| c.id.clone()),
            scope: UPLOAD_SCOPE,
            storage: if self.keychain {
                "keychain"
            } else {
                "owner_only_file"
            },
            access_token_expired: credentials.as_ref().is_some_and(|c| c.expires_at <= now()),
        })
    }

    // Held during mutations/refresh/upload. A second writer fails instead of racing
    // a rotated token, replacing an account, or sending duplicate upload chunks.
    pub(super) fn lock(&self) -> Result<File> {
        let parent = self.path.parent().context("credential parent directory")?;
        if !parent.exists() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(parent)
                .context("create private credential directory")?;
        }
        let file = private_open(&self.path.with_extension("lock"), true)?;
        file.try_lock_exclusive().map_err(|_| super::YoutubeError {
            code: "youtube.credential_busy",
            message: "Another YouTube operation holds this credential; retry after it finishes",
        })?;
        Ok(file)
    }

    pub(super) fn load_optional(&self) -> Result<Option<Credentials>> {
        #[cfg(target_os = "macos")]
        if self.keychain {
            let entry = keyring::Entry::new("com.rcd.vlogcutter.youtube", "upload")?;
            return match entry.get_password() {
                Ok(value) => parse(&value).map(Some),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(_) => failure(
                    "youtube.credential_error",
                    "Unable to read YouTube credentials from Keychain",
                ),
            };
        }
        let file = match private_open(&self.path, false) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let mut value = String::new();
        file.take(65537)
            .read_to_string(&mut value)
            .context("read YouTube credential file")?;
        ensure!(
            value.len() <= 65536,
            "invalid input: credential file exceeds size limit"
        );
        parse(&value).map(Some)
    }

    pub(super) fn load(&self) -> Result<Credentials> {
        self.load_optional()?.ok_or_else(|| {
            super::YoutubeError {
                code: "youtube.auth_required",
                message: "Authorize uploads first with youtube auth login",
            }
            .into()
        })
    }

    pub(super) fn save(&self, credentials: &Credentials) -> Result<()> {
        exact_scope(&credentials.scope)?;
        let value = serde_json::to_string(credentials)?;
        #[cfg(target_os = "macos")]
        if self.keychain {
            return keyring::Entry::new("com.rcd.vlogcutter.youtube", "upload")?
                .set_password(&value)
                .map_err(|_| {
                    super::YoutubeError {
                        code: "youtube.credential_error",
                        message: "Unable to save YouTube credentials in Keychain",
                    }
                    .into()
                });
        }
        let temporary = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut file = private_create(&temporary)?;
            file.write_all(value.as_bytes())
                .context("write YouTube credentials")?;
            file.sync_all().context("sync YouTube credentials")?;
            // Refuse an existing symlink or insecure destination even though rename
            // would replace it. Never follow it or weaken its permissions.
            if self.path.symlink_metadata().is_ok() {
                private_open(&self.path, false)?;
            }
            fs::rename(&temporary, &self.path).context("commit YouTube credentials")?;
            File::open(self.path.parent().unwrap())?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub(super) fn delete(&self) -> Result<()> {
        #[cfg(target_os = "macos")]
        if self.keychain {
            return keyring::Entry::new("com.rcd.vlogcutter.youtube", "upload")?
                .delete_credential()
                .map_err(|_| {
                    super::YoutubeError {
                        code: "youtube.credential_error",
                        message: "Unable to delete YouTube credentials from Keychain",
                    }
                    .into()
                });
        }
        fs::remove_file(&self.path).context("delete revoked YouTube credentials")
    }
}

fn parse(value: &str) -> Result<Credentials> {
    let credentials: Credentials =
        serde_json::from_str(value).map_err(|_| super::YoutubeError {
            code: "youtube.credential_error",
            message: "Invalid YouTube credential file",
        })?;
    exact_scope(&credentials.scope)?;
    ensure!(
        !credentials.id.is_empty()
            && !credentials.client_id.is_empty()
            && !credentials.refresh_token.is_empty()
            && !credentials.access_token.is_empty(),
        "invalid input: incomplete YouTube credential"
    );
    Ok(credentials)
}

fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options
}

fn private_create(path: &Path) -> Result<File> {
    options()
        .write(true)
        .create_new(true)
        .open(path)
        .context("create private credential staging file")
}

fn private_open(path: &Path, create: bool) -> Result<File> {
    let mut options = options();
    options.read(true);
    if create {
        options.write(true).create(true);
    }
    let file = options
        .open(path)
        .context("open private YouTube credential file")?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file(),
        "invalid input: credential must be a regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.mode() & 0o077 == 0 && metadata.uid() == unsafe { libc::geteuid() },
            "invalid input: YouTube credential and lock files must be owned by you with permissions 0600"
        );
    }
    Ok(file)
}
