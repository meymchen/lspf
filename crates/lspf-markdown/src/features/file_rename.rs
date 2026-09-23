//! Link updates when files or directories move.

use std::str::FromStr;
use std::sync::Arc;

use lspf::types::{RenameFilesParams, TextEdit, Uri, WorkspaceEdit};
use lspf::{CancellationToken, LspError, ServerContext};

use crate::State;
use crate::features::hrefs;
use crate::features::references::names;
use crate::features::rename::Edits;
use crate::index::{WorkspaceIndex, is_markdown_path};
use crate::target::{
    encode_path, file_name, percent_decode, relative_path, resolve_local_target,
    root_relative_path, uri_key, without_fragment,
};

/// Where `uri` lives after `renames`, when one of them moves it.
fn moved(uri: &Uri, renames: &[(Uri, Uri)]) -> Option<Uri> {
    let key = uri_key(uri);
    for (old, new) in renames {
        if names(uri, old) {
            return Some(new.clone());
        }
        if let Some(rest) = key.strip_prefix(&format!("{}/", uri_key(old))) {
            let base = without_fragment(new.as_str()).trim_end_matches('/');
            return Uri::from_str(&format!("{base}/{}", encode_path(rest))).ok();
        }
    }
    None
}

/// Spell the path to `target` from `source` in the style of `original`.
fn respell(original: &str, source: &Uri, target: &Uri, root: Option<&Uri>) -> Option<String> {
    let mut path = if original.starts_with("file:") {
        target.as_str().to_string()
    } else if original.starts_with('/') {
        root_relative_path(root?, target)?
    } else {
        let relative = relative_path(source, target)?;
        if original.starts_with("./") && !relative.starts_with("..") {
            format!("./{relative}")
        } else {
            relative
        }
    };
    let original_name = original.rsplit('/').next().unwrap_or(original);
    if !original_name.contains('.') && is_markdown_path(target) {
        let name = file_name(target);
        if let Some((stem, _)) = name.rsplit_once('.') {
            let encoded_len = encode_path(&name).len();
            let stem_len = encode_path(stem).len();
            if path.len() >= encoded_len {
                path.truncate(path.len() - (encoded_len - stem_len));
            }
        }
    }
    if !original.contains('%') {
        let decoded = percent_decode(&path);
        if !decoded
            .contains(|character: char| character.is_whitespace() || "<>".contains(character))
        {
            path = decoded;
        }
    }
    Some(path)
}

/// Text edits that keep every workspace link pointing at its resource after
/// `renames` move files or directories.
pub(crate) async fn link_edits(
    state: &State,
    ctx: &ServerContext,
    renames: &[(Uri, Uri)],
) -> Edits {
    let mut edits = Edits::default();
    for entry in state.index.all(ctx).await {
        let source = entry.uri().clone();
        let source_moved = moved(&source, renames);
        let new_source = source_moved.clone().unwrap_or_else(|| source.clone());
        let root = WorkspaceIndex::root_for(ctx, &source);
        for href in hrefs(&entry) {
            let path_range = href.path_range();
            if path_range.is_empty() || crate::target::is_external(&href.text) {
                continue;
            }
            let Some(target) = resolve_local_target(&source, &href.text, root.as_ref()) else {
                continue;
            };
            let target_moved = moved(&target.uri, renames);
            if target_moved.is_none() && source_moved.is_none() {
                continue;
            }
            let original = &href.text[..path_range.end - path_range.start];
            if source_moved.is_some()
                && target_moved.is_none()
                && (original.starts_with('/') || original.starts_with("file:"))
            {
                continue;
            }
            let actual = match &target_moved {
                Some(uri) => uri.clone(),
                None => target.uri.clone(),
            };
            let Some(path) = respell(original, &new_source, &actual, root.as_ref()) else {
                continue;
            };
            if path == original {
                continue;
            }
            if let Some(range) = entry.range(&path_range) {
                edits.push(
                    &source,
                    TextEdit {
                        range,
                        new_text: path,
                    },
                );
            }
        }
    }
    edits
}

pub(crate) async fn will_rename_files(
    state: Arc<State>,
    ctx: ServerContext,
    params: RenameFilesParams,
    _ct: CancellationToken,
) -> Result<Option<WorkspaceEdit>, LspError> {
    let renames: Vec<(Uri, Uri)> = params
        .files
        .into_iter()
        .map(|file| (file.old_uri, file.new_uri))
        .collect();
    let edits = link_edits(&state, &ctx, &renames).await;
    Ok((!edits.is_empty()).then(|| edits.into_workspace_edit(Vec::new())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(value: &str) -> Uri {
        Uri::from_str(value).unwrap()
    }

    #[test]
    fn moves_cover_files_and_directory_contents() {
        let renames = [
            (uri("file:///w/a.md"), uri("file:///w/b.md")),
            (uri("file:///w/docs"), uri("file:///w/manual")),
        ];
        assert_eq!(
            moved(&uri("file:///w/a.md"), &renames).unwrap().as_str(),
            "file:///w/b.md"
        );
        assert_eq!(
            moved(&uri("file:///w/a"), &renames).unwrap().as_str(),
            "file:///w/b.md"
        );
        assert_eq!(
            moved(&uri("file:///w/docs/x%20y.md"), &renames)
                .unwrap()
                .as_str(),
            "file:///w/manual/x%20y.md"
        );
        assert!(moved(&uri("file:///w/docs2/x.md"), &renames).is_none());
    }

    #[test]
    fn respelling_keeps_the_link_style() {
        let source = uri("file:///w/docs/readme.md");
        let root = uri("file:///w");
        let target = uri("file:///w/guide/new%20name.md");
        assert_eq!(
            respell("../old.md", &source, &target, Some(&root)).as_deref(),
            Some("../guide/new%20name.md")
        );
        assert_eq!(
            respell("/old.md", &source, &target, Some(&root)).as_deref(),
            Some("/guide/new%20name.md")
        );
        assert_eq!(
            respell("file:///w/old.md", &source, &target, Some(&root)).as_deref(),
            Some("file:///w/guide/new%20name.md")
        );
        let plain = uri("file:///w/docs/%E4%B8%AD%E6%96%87.md");
        assert_eq!(
            respell("./old", &source, &plain, Some(&root)).as_deref(),
            Some("./中文")
        );
    }
}
