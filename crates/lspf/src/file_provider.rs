//! Replaceable text-resource lookup for unopened documents.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use lsp_types::Uri;

use crate::runtime::TaskSend;
use crate::uri_key::UriKey;

/// Supplies text for resources that are not open in the editor.
///
/// The returned future is `Send` on native targets and may remain local on
/// browser WASM, matching the framework's runtime task boundary.
pub trait FileProvider: Send + Sync + 'static {
    fn read_text(&self, uri: &Uri) -> impl Future<Output = Option<String>> + TaskSend;
}

#[cfg(not(target_arch = "wasm32"))]
type ProviderFuture<'a> = Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>>;

#[cfg(target_arch = "wasm32")]
type ProviderFuture<'a> = Pin<Box<dyn Future<Output = Option<String>> + 'a>>;

pub(crate) trait ErasedFileProvider: Send + Sync {
    fn read_text<'a>(&'a self, uri: &'a Uri) -> ProviderFuture<'a>;
}

struct FileProviderAdapter<P>(P);

impl<P: FileProvider> ErasedFileProvider for FileProviderAdapter<P> {
    fn read_text<'a>(&'a self, uri: &'a Uri) -> ProviderFuture<'a> {
        Box::pin(self.0.read_text(uri))
    }
}

pub(crate) type SharedFileProvider = Arc<dyn ErasedFileProvider>;

pub(crate) fn erase(provider: impl FileProvider) -> SharedFileProvider {
    Arc::new(FileProviderAdapter(provider))
}

/// An in-memory [`FileProvider`] intended for tests and virtual resources.
///
/// Clones share one backing store. URI lookup uses the same normalized
/// identity as the connection's open documents.
#[derive(Debug, Clone, Default)]
pub struct MemoryFileProvider {
    entries: Arc<RwLock<HashMap<UriKey, String>>>,
}

impl MemoryFileProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace the text associated with `uri`.
    pub fn insert(&self, uri: impl Borrow<Uri>, text: impl Into<String>) -> Option<String> {
        self.entries
            .write()
            .unwrap()
            .insert(UriKey::new(uri.borrow()), text.into())
    }

    /// Remove and return the text associated with `uri`, if present.
    pub fn remove(&self, uri: &Uri) -> Option<String> {
        self.entries.write().unwrap().remove(&UriKey::new(uri))
    }
}

impl FileProvider for MemoryFileProvider {
    async fn read_text(&self, uri: &Uri) -> Option<String> {
        self.entries.read().unwrap().get(&UriKey::new(uri)).cloned()
    }
}
