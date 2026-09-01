use lspf::types::{
    ChangeNotifications, WorkspaceFoldersServerCapabilities, WorkspaceServerCapabilities,
};

pub(crate) fn workspace_capabilities() -> WorkspaceServerCapabilities {
    WorkspaceServerCapabilities {
        workspace_folders: Some(WorkspaceFoldersServerCapabilities {
            supported: Some(true),
            change_notifications: Some(ChangeNotifications::Bool(true)),
        }),
        ..WorkspaceServerCapabilities::default()
    }
}
