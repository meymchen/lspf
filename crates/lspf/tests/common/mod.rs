use lspf::types::{OneOf, WorkspaceFoldersServerCapabilities, WorkspaceServerCapabilities};

pub(crate) fn workspace_capabilities() -> WorkspaceServerCapabilities {
    WorkspaceServerCapabilities {
        workspace_folders: Some(WorkspaceFoldersServerCapabilities {
            supported: Some(true),
            change_notifications: Some(OneOf::Left(true)),
        }),
        ..WorkspaceServerCapabilities::default()
    }
}
