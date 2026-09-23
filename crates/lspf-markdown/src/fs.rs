//! The workspace file system the Markdown service reads beyond open documents.
//!
//! lspf's [`FileProvider`] reads the text of one resource. Cross-file
//! features also need to know whether a non-text target such as an image
//! exists and which files a directory holds, so [`WorkspaceFs`] extends the
//! provider with `stat` and `read_directory`.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use lspf::types::Uri;
use lspf::{FileProvider, OsFileProvider, WorkspaceError};

use crate::target::{percent_decode, uri_key};

/// What a workspace path names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
}

/// A [`FileProvider`] that can also stat resources and list directories.
pub trait WorkspaceFs: FileProvider + Clone {
    /// Report what `uri` names, or `None` when nothing exists there.
    fn stat(&self, uri: &Uri) -> impl Future<Output = Option<FileKind>> + Send;

    /// List the entry names and kinds directly inside the directory `uri`.
    fn read_directory(&self, uri: &Uri) -> impl Future<Output = Vec<(String, FileKind)>> + Send;
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The object-safe form of [`WorkspaceFs`] the server state keeps.
pub(crate) trait DynFs: Send + Sync {
    fn stat<'a>(&'a self, uri: &'a Uri) -> BoxFuture<'a, Option<FileKind>>;
    fn read_directory<'a>(&'a self, uri: &'a Uri) -> BoxFuture<'a, Vec<(String, FileKind)>>;
}

impl<F: WorkspaceFs> DynFs for F {
    fn stat<'a>(&'a self, uri: &'a Uri) -> BoxFuture<'a, Option<FileKind>> {
        Box::pin(WorkspaceFs::stat(self, uri))
    }

    fn read_directory<'a>(&'a self, uri: &'a Uri) -> BoxFuture<'a, Vec<(String, FileKind)>> {
        Box::pin(WorkspaceFs::read_directory(self, uri))
    }
}

/// The native file system, for `file:` URIs.
#[derive(Debug, Clone, Default)]
pub struct OsFs {
    files: OsFileProvider,
}

impl OsFs {
    /// Read the native file system with lspf's default size limit.
    pub fn new() -> Self {
        Self::default()
    }
}

fn native_path(uri: &Uri) -> Option<PathBuf> {
    if !uri.scheme().as_str().eq_ignore_ascii_case("file") {
        return None;
    }
    let host = uri
        .authority()
        .map(|authority| authority.host().to_string())
        .unwrap_or_default();
    let path = percent_decode(uri.path().as_str());
    if cfg!(windows) {
        if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
            return Some(PathBuf::from(format!(
                r"\\{host}{}",
                path.replace('/', "\\")
            )));
        }
        let path = path.strip_prefix('/')?;
        let drive = path.as_bytes();
        (drive.len() >= 2 && drive[0].is_ascii_alphabetic() && drive[1] == b':')
            .then(|| PathBuf::from(path.replace('/', "\\")))
    } else {
        (host.is_empty() || host.eq_ignore_ascii_case("localhost"))
            .then_some(path)
            .filter(|path| path.starts_with('/'))
            .map(PathBuf::from)
    }
}

impl FileProvider for OsFs {
    async fn read_text(&self, uri: &Uri) -> Result<Option<String>, WorkspaceError> {
        self.files.read_text(uri).await
    }
}

impl WorkspaceFs for OsFs {
    async fn stat(&self, uri: &Uri) -> Option<FileKind> {
        let metadata = tokio::fs::metadata(native_path(uri)?).await.ok()?;
        Some(if metadata.is_dir() {
            FileKind::Directory
        } else {
            FileKind::File
        })
    }

    async fn read_directory(&self, uri: &Uri) -> Vec<(String, FileKind)> {
        let Some(path) = native_path(uri) else {
            return Vec::new();
        };
        let Ok(mut entries) = tokio::fs::read_dir(path).await else {
            return Vec::new();
        };
        let mut listing = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let kind = if file_type.is_dir() {
                FileKind::Directory
            } else {
                FileKind::File
            };
            listing.push((name, kind));
        }
        listing.sort();
        listing
    }
}

/// An in-memory workspace for tests and virtual resources. Clones share one
/// store; directories exist implicitly above every inserted file.
#[derive(Debug, Clone, Default)]
pub struct MemoryFs {
    files: Arc<RwLock<BTreeMap<String, Option<String>>>>,
}

impl MemoryFs {
    /// Create an empty workspace.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a text file.
    pub fn insert(&self, uri: Uri, text: impl Into<String>) {
        self.files
            .write()
            .expect("the memory file system lock is not poisoned")
            .insert(uri_key(&uri), Some(text.into()));
    }

    /// Insert a file with no text content, such as an image.
    pub fn insert_binary(&self, uri: Uri) {
        self.files
            .write()
            .expect("the memory file system lock is not poisoned")
            .insert(uri_key(&uri), None);
    }

    /// Remove a file.
    pub fn remove(&self, uri: &Uri) {
        self.files
            .write()
            .expect("the memory file system lock is not poisoned")
            .remove(&uri_key(uri));
    }
}

impl FileProvider for MemoryFs {
    async fn read_text(&self, uri: &Uri) -> Result<Option<String>, WorkspaceError> {
        let files = self
            .files
            .read()
            .expect("the memory file system lock is not poisoned");
        match files.get(&uri_key(uri)) {
            Some(Some(text)) => Ok(Some(text.clone())),
            Some(None) => Err(WorkspaceError::InvalidEncoding),
            None => Ok(None),
        }
    }
}

impl WorkspaceFs for MemoryFs {
    async fn stat(&self, uri: &Uri) -> Option<FileKind> {
        let key = uri_key(uri);
        let files = self
            .files
            .read()
            .expect("the memory file system lock is not poisoned");
        if files.contains_key(&key) {
            return Some(FileKind::File);
        }
        let prefix = format!("{key}/");
        files
            .range(prefix.clone()..)
            .next()
            .filter(|(candidate, _)| candidate.starts_with(&prefix))
            .map(|_| FileKind::Directory)
    }

    async fn read_directory(&self, uri: &Uri) -> Vec<(String, FileKind)> {
        let prefix = format!("{}/", uri_key(uri));
        let files = self
            .files
            .read()
            .expect("the memory file system lock is not poisoned");
        let mut listing: Vec<(String, FileKind)> = Vec::new();
        for key in files.range(prefix.clone()..).map(|(key, _)| key) {
            let Some(rest) = key.strip_prefix(&prefix) else {
                break;
            };
            let entry = match rest.split_once('/') {
                Some((directory, _)) => (directory.to_string(), FileKind::Directory),
                None => (rest.to_string(), FileKind::File),
            };
            if !listing.contains(&entry) {
                listing.push(entry);
            }
        }
        listing
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn uri(value: &str) -> Uri {
        Uri::from_str(value).unwrap()
    }

    #[tokio::test]
    async fn memory_directories_are_implied_by_their_files() {
        let fs = MemoryFs::new();
        fs.insert(uri("file:///w/readme.md"), "# Readme\n");
        fs.insert(uri("file:///w/docs/guide.md"), "# Guide\n");
        fs.insert_binary(uri("file:///w/docs/img/logo.png"));

        assert_eq!(
            WorkspaceFs::stat(&fs, &uri("file:///w/docs")).await,
            Some(FileKind::Directory)
        );
        assert_eq!(
            WorkspaceFs::stat(&fs, &uri("file:///w/docs/img/logo.png")).await,
            Some(FileKind::File)
        );
        assert_eq!(
            WorkspaceFs::stat(&fs, &uri("file:///w/missing")).await,
            None
        );
        assert_eq!(
            WorkspaceFs::read_directory(&fs, &uri("file:///w/docs/")).await,
            [
                ("guide.md".to_string(), FileKind::File),
                ("img".to_string(), FileKind::Directory),
            ]
        );
        assert!(matches!(
            fs.read_text(&uri("file:///w/docs/img/logo.png")).await,
            Err(WorkspaceError::InvalidEncoding)
        ));
        fs.remove(&uri("file:///w/readme.md"));
        assert_eq!(
            fs.read_text(&uri("file:///w/readme.md")).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn os_fs_reads_real_directories() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a.md"), "# A\n").unwrap();
        std::fs::create_dir(directory.path().join("sub")).unwrap();
        let root = file_uri(directory.path());
        let fs = OsFs::new();

        assert_eq!(
            WorkspaceFs::stat(&fs, &root).await,
            Some(FileKind::Directory)
        );
        assert_eq!(
            WorkspaceFs::read_directory(&fs, &root).await,
            [
                ("a.md".to_string(), FileKind::File),
                ("sub".to_string(), FileKind::Directory),
            ]
        );
        let file = crate::target::child(&root, "a.md").unwrap();
        assert_eq!(WorkspaceFs::stat(&fs, &file).await, Some(FileKind::File));
        assert_eq!(fs.read_text(&file).await.unwrap().as_deref(), Some("# A\n"));
        assert_eq!(
            WorkspaceFs::stat(&fs, &uri("https://example.com/a.md")).await,
            None
        );
    }

    fn file_uri(path: &std::path::Path) -> Uri {
        let path = path.to_string_lossy().replace('\\', "/");
        let path = if path.starts_with('/') {
            path
        } else {
            format!("/{path}")
        };
        uri(&format!("file://{}", crate::target::encode_path(&path)))
    }
}
