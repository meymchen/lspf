//! The workspace index: parsed documents and the Markdown files they live in.
//!
//! lspf cannot list open documents or workspace files, so the index tracks
//! the URIs the client opened and walks the workspace roots through
//! [`WorkspaceFs`](crate::fs::WorkspaceFs). Parsed documents are cached by
//! version while open. Unopened files are cached only when the client
//! delivers file-change notifications; otherwise every lookup rereads them.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lspf::types::{LogMessageParams, MessageType, Position, Range, Uri};
use lspf::{Document, ServerContext};

use crate::fs::{DynFs, FileKind};
use crate::parse::{MdDocument, parse};
use crate::target::{LocalTarget, child, is_within, resolve_local_target, uri_key};

/// File extensions treated as Markdown, the first being the default.
pub(crate) const MARKDOWN_EXTENSIONS: [&str; 5] = ["md", "markdown", "mdown", "mkd", "mkdn"];

/// The most Markdown files one workspace walk collects.
const MAX_FILES: usize = 10_000;

/// One parsed document and the text snapshot its offsets refer to.
pub(crate) struct Entry {
    pub(crate) document: Document,
    pub(crate) md: MdDocument,
}

impl Entry {
    pub(crate) fn uri(&self) -> &Uri {
        self.document.uri()
    }

    pub(crate) fn range(&self, range: &std::ops::Range<usize>) -> Option<Range> {
        Some(Range::new(
            self.document.offset_to_position(range.start)?,
            self.document.offset_to_position(range.end)?,
        ))
    }

    pub(crate) fn position(&self, offset: usize) -> Option<Position> {
        self.document.offset_to_position(offset)
    }

    pub(crate) fn offset(&self, position: Position) -> Option<usize> {
        self.document.position_to_offset(position)
    }

    pub(crate) fn text(&self) -> String {
        self.document.text(None).into_owned()
    }
}

/// A link destination resolved against the workspace.
pub(crate) struct Resolved {
    pub(crate) target: LocalTarget,
    pub(crate) kind: Option<FileKind>,
}

struct Cached {
    version: Option<i32>,
    entry: Arc<Entry>,
}

pub(crate) struct WorkspaceIndex {
    fs: Arc<dyn DynFs>,
    cache: Mutex<HashMap<String, Cached>>,
    open: Mutex<HashMap<String, Uri>>,
    files: Mutex<Option<Vec<Uri>>>,
    cache_unopened: AtomicBool,
}

pub(crate) fn is_markdown_path(uri: &Uri) -> bool {
    let name = crate::target::file_name(uri);
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        MARKDOWN_EXTENSIONS
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    })
}

fn excluded(name: &str) -> bool {
    name.starts_with('.') || name == "node_modules"
}

impl WorkspaceIndex {
    pub(crate) fn new(fs: Arc<dyn DynFs>) -> Self {
        Self {
            fs,
            cache: Mutex::default(),
            open: Mutex::default(),
            files: Mutex::default(),
            cache_unopened: AtomicBool::new(false),
        }
    }

    pub(crate) fn fs(&self) -> &dyn DynFs {
        self.fs.as_ref()
    }

    /// Cache unopened files from now on; the client reports their changes.
    pub(crate) fn watching(&self) {
        self.cache_unopened.store(true, Ordering::Release);
    }

    pub(crate) fn opened(&self, uri: &Uri) {
        let key = uri_key(uri);
        self.lock_open().insert(key.clone(), uri.clone());
        self.lock_cache().remove(&key);
    }

    pub(crate) fn closed(&self, uri: &Uri) {
        let key = uri_key(uri);
        self.lock_open().remove(&key);
        self.lock_cache().remove(&key);
    }

    pub(crate) fn open_uris(&self) -> Vec<Uri> {
        self.lock_open().values().cloned().collect()
    }

    pub(crate) fn is_open(&self, uri: &Uri) -> bool {
        self.lock_open().contains_key(&uri_key(uri))
    }

    /// Forget a changed unopened file, and the file list when files came or
    /// went.
    pub(crate) fn changed(&self, uri: &Uri, listing_changed: bool) {
        let key = uri_key(uri);
        let open = self.lock_open().contains_key(&key);
        if !open {
            self.lock_cache().remove(&key);
        }
        if listing_changed {
            *self.lock_files() = None;
        }
    }

    /// Forget every cached unopened file and the file list.
    pub(crate) fn reset(&self) {
        let open = self.lock_open().clone();
        self.lock_cache().retain(|key, _| open.contains_key(key));
        *self.lock_files() = None;
    }

    fn lock_cache(&self) -> std::sync::MutexGuard<'_, HashMap<String, Cached>> {
        self.cache
            .lock()
            .expect("the document cache lock is not poisoned")
    }

    fn lock_open(&self) -> std::sync::MutexGuard<'_, HashMap<String, Uri>> {
        self.open
            .lock()
            .expect("the open-document lock is not poisoned")
    }

    fn lock_files(&self) -> std::sync::MutexGuard<'_, Option<Vec<Uri>>> {
        self.files
            .lock()
            .expect("the file-list lock is not poisoned")
    }

    /// The parsed document at `uri`: the open snapshot when there is one,
    /// otherwise the file's current text.
    pub(crate) async fn get(&self, ctx: &ServerContext, uri: &Uri) -> Option<Arc<Entry>> {
        let key = uri_key(uri);
        let open = ctx.documents().get(uri);
        let version = open.as_ref().and_then(Document::version);
        if let Some(cached) = self.lock_cache().get(&key)
            && cached.version == version
            && (open.is_some() || self.cache_unopened.load(Ordering::Acquire))
        {
            return Some(Arc::clone(&cached.entry));
        }
        let document = match open {
            Some(document) => document,
            None => ctx.workspace().text_document(uri).await.ok()?,
        };
        let md = parse(&document.text(None));
        let entry = Arc::new(Entry { document, md });
        if version.is_some() || self.cache_unopened.load(Ordering::Acquire) {
            self.lock_cache().insert(
                key,
                Cached {
                    version,
                    entry: Arc::clone(&entry),
                },
            );
        }
        Some(entry)
    }

    /// The workspace root holding `uri`, if any.
    pub(crate) fn root_for(ctx: &ServerContext, uri: &Uri) -> Option<Uri> {
        ctx.workspace()
            .roots()
            .into_iter()
            .map(|folder| folder.uri)
            .filter(|root| is_within(uri, root))
            .max_by_key(|root| root.as_str().len())
    }

    /// Resolve a link destination written in `source` and report what exists
    /// there. An extensionless path that names no resource falls back to the
    /// same path with the default Markdown extension.
    pub(crate) async fn resolve(
        &self,
        ctx: &ServerContext,
        source: &Uri,
        href: &str,
    ) -> Option<Resolved> {
        let root = Self::root_for(ctx, source);
        let target = resolve_local_target(source, href, root.as_ref())?;
        let kind = self.stat(ctx, &target.uri).await;
        if kind.is_none() && !crate::target::file_name(&target.uri).contains('.') {
            let with_extension = LocalTarget {
                uri: format!("{}.{}", target.uri.as_str(), MARKDOWN_EXTENSIONS[0])
                    .parse()
                    .ok()?,
                fragment: target.fragment.clone(),
            };
            if let Some(kind) = self.stat(ctx, &with_extension.uri).await {
                return Some(Resolved {
                    target: with_extension,
                    kind: Some(kind),
                });
            }
        }
        Some(Resolved { target, kind })
    }

    async fn stat(&self, ctx: &ServerContext, uri: &Uri) -> Option<FileKind> {
        if self.is_open(uri) || ctx.documents().get(uri).is_some() {
            return Some(FileKind::File);
        }
        self.fs.stat(uri).await
    }

    /// Every Markdown file in the workspace roots plus every open document.
    pub(crate) async fn markdown_files(&self, ctx: &ServerContext) -> Vec<Uri> {
        let cached = self.lock_files().clone();
        let mut files = match cached {
            Some(files) => files,
            None => {
                let files = self.walk(ctx).await;
                *self.lock_files() = Some(files.clone());
                files
            }
        };
        let known: std::collections::HashSet<String> = files.iter().map(uri_key).collect();
        files.extend(
            self.open_uris()
                .into_iter()
                .filter(|uri| !known.contains(&uri_key(uri))),
        );
        files
    }

    async fn walk(&self, ctx: &ServerContext) -> Vec<Uri> {
        let mut pending: VecDeque<Uri> = ctx
            .workspace()
            .roots()
            .into_iter()
            .map(|folder| folder.uri)
            .collect();
        let mut files = Vec::new();
        while let Some(directory) = pending.pop_front() {
            for (name, kind) in self.fs.read_directory(&directory).await {
                if excluded(&name) {
                    continue;
                }
                let Some(uri) = child(&directory, &name) else {
                    continue;
                };
                match kind {
                    FileKind::Directory => pending.push_back(uri),
                    FileKind::File if is_markdown_path(&uri) => {
                        files.push(uri);
                        if files.len() >= MAX_FILES {
                            let _ = ctx.client().log_message(LogMessageParams {
                                kind: MessageType::Warning,
                                message: format!(
                                    "lspf-markdown indexes at most {MAX_FILES} Markdown files; \
                                     cross-file features ignore the rest"
                                ),
                            });
                            return files;
                        }
                    }
                    FileKind::File => {}
                }
            }
        }
        files
    }

    /// Parse every Markdown file in the workspace.
    pub(crate) async fn all(&self, ctx: &ServerContext) -> Vec<Arc<Entry>> {
        let mut entries = Vec::new();
        for uri in self.markdown_files(ctx).await {
            if let Some(entry) = self.get(ctx, &uri).await {
                entries.push(entry);
            }
        }
        entries
    }
}
