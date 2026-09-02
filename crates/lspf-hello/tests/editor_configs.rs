//! Regression tests for the checked-in VS Code and Zed project configuration.

use serde_json::{Value, json};

const GITIGNORE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.gitignore"));
const VSCODE_EXTENSIONS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.vscode/extensions.json"
));
const VSCODE_LAUNCH: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.vscode/launch.json"
));
const VSCODE_TASKS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.vscode/tasks.json"
));
const TEST_CLIENT_LAUNCH: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tools/vscode-test-client/.vscode/launch.json"
));
const TEST_CLIENT_TASKS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tools/vscode-test-client/.vscode/tasks.json"
));
const ZED_DEBUG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.zed/debug.json"
));
const ZED_TASKS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.zed/tasks.json"
));

fn parse(name: &str, source: &str) -> Value {
    serde_json::from_str(source)
        .unwrap_or_else(|error| panic!("{name} must be valid JSON: {error}"))
}

fn find_by<'a>(items: &'a [Value], field: &str, value: &str) -> &'a Value {
    items
        .iter()
        .find(|item| item[field] == value)
        .unwrap_or_else(|| panic!("missing {field} `{value}`"))
}

fn object_array<'a>(document: &'a Value, field: &str) -> &'a [Value] {
    document[field]
        .as_array()
        .unwrap_or_else(|| panic!("`{field}` must be an array"))
}

#[test]
fn root_editor_configuration_is_tracked_and_parseable() {
    assert!(
        !GITIGNORE.lines().any(|line| line == "/.vscode/"),
        "the root VS Code project configuration must not be ignored"
    );

    for (name, source) in [
        (".vscode/extensions.json", VSCODE_EXTENSIONS),
        (".vscode/launch.json", VSCODE_LAUNCH),
        (".vscode/tasks.json", VSCODE_TASKS),
        (".zed/debug.json", ZED_DEBUG),
        (".zed/tasks.json", ZED_TASKS),
    ] {
        parse(name, source);
    }
}

#[test]
fn vscode_has_quick_full_and_example_tasks() {
    let document = parse(".vscode/tasks.json", VSCODE_TASKS);
    let tasks = object_array(&document, "tasks");

    assert_eq!(
        find_by(tasks, "label", "test lspf-hello (quick)")["args"],
        json!(["test", "-p", "lspf-hello"])
    );
    assert_eq!(
        find_by(tasks, "label", "test workspace (full)")["args"],
        json!([
            "test",
            "--workspace",
            "--features",
            "stdio,tcp,websocket",
            "--all-targets"
        ])
    );
    assert!(
        find_by(tasks, "label", "run LSP example")["args"]
            .as_array()
            .is_some_and(|args| args.iter().any(|arg| arg == "${input:exampleName}")),
        "the VS Code example task must prompt for the example name"
    );
}

#[test]
fn vscode_extension_host_launch_builds_server_first() {
    let launch = parse(".vscode/launch.json", VSCODE_LAUNCH);
    let configurations = object_array(&launch, "configurations");
    let smoke = find_by(configurations, "name", "Debug LSP client (Extension Host)");
    assert_eq!(smoke["preLaunchTask"], "prepare VS Code LSP smoke");
    assert!(
        object_array(smoke, "args")
            .iter()
            .any(|arg| arg == "--disable-extensions"),
        "the Extension Host must isolate the test client from installed extensions"
    );
    let example = find_by(
        configurations,
        "name",
        "Run LSP example client (select example)",
    );
    assert_eq!(example["env"]["LSPF_TEST_EXAMPLE"], "${input:exampleName}");
    assert_eq!(example["preLaunchTask"], "prepare VS Code LSP example");
    let attach = find_by(
        configurations,
        "name",
        "Attach to running LSP server/example",
    );
    assert_eq!(attach["request"], "attach");
    assert_eq!(attach["pid"], "${command:pickMyProcess}");
    let tasks = parse(".vscode/tasks.json", VSCODE_TASKS);
    let prepare = find_by(
        object_array(&tasks, "tasks"),
        "label",
        "prepare VS Code LSP smoke",
    );
    assert_eq!(
        prepare["dependsOn"],
        json!([
            "ensure VS Code test client dependencies",
            "build lspf-hello",
            "watch VS Code test client"
        ])
    );
    let ensure = find_by(
        object_array(&tasks, "tasks"),
        "label",
        "ensure VS Code test client dependencies",
    );
    assert_eq!(ensure["script"], "prepare:debug");
    let prepare_example = find_by(
        object_array(&tasks, "tasks"),
        "label",
        "prepare VS Code LSP example",
    );
    assert_eq!(
        prepare_example["dependsOn"],
        json!([
            "ensure VS Code test client dependencies",
            "build LSP examples",
            "watch VS Code test client"
        ])
    );

    let nested_launch = parse("test client launch.json", TEST_CLIENT_LAUNCH);
    let nested_smoke = find_by(
        object_array(&nested_launch, "configurations"),
        "name",
        "Debug LSP client (Extension Host)",
    );
    assert_eq!(nested_smoke["preLaunchTask"], "prepare VS Code LSP smoke");
    assert!(
        object_array(nested_smoke, "args")
            .iter()
            .any(|arg| arg == "--disable-extensions"),
        "the nested Extension Host must isolate the test client from installed extensions"
    );
    let nested_example = find_by(
        object_array(&nested_launch, "configurations"),
        "name",
        "Run LSP example client (select example)",
    );
    assert_eq!(
        nested_example["env"]["LSPF_TEST_EXAMPLE"],
        "${input:exampleName}"
    );

    let nested_tasks = parse("test client tasks.json", TEST_CLIENT_TASKS);
    let nested_prepare = find_by(
        object_array(&nested_tasks, "tasks"),
        "label",
        "prepare VS Code LSP smoke",
    );
    assert_eq!(
        nested_prepare["dependsOn"],
        json!([
            "ensure VS Code test client dependencies",
            "build lspf-hello",
            "watch VS Code test client"
        ])
    );
}

#[test]
fn zed_has_quick_full_run_and_debug_entries() {
    let tasks = parse(".zed/tasks.json", ZED_TASKS);
    let tasks = tasks.as_array().expect("Zed tasks must be an array");
    assert_eq!(
        find_by(tasks, "label", "test lspf-hello (quick)")["args"],
        json!(["test", "-p", "lspf-hello"])
    );
    assert_eq!(
        find_by(tasks, "label", "test workspace (full)")["args"],
        json!([
            "test",
            "--workspace",
            "--features",
            "stdio,tcp,websocket",
            "--all-targets"
        ])
    );
    assert_eq!(
        find_by(tasks, "label", "run hover example (waits for stdio client)")["args"],
        json!(["run", "-p", "lspf", "--example", "hover"])
    );

    let debug = parse(".zed/debug.json", ZED_DEBUG);
    let debug = debug
        .as_array()
        .expect("Zed debug configurations must be an array");
    let attach = find_by(debug, "label", "Attach to running LSP server/example");
    assert_eq!(attach["adapter"], "CodeLLDB");
    assert_eq!(attach["request"], "attach");
    assert_eq!(attach["sourceLanguages"], json!(["rust"]));
}

#[test]
fn vscode_recommends_rust_analyzer_and_codelldb() {
    let extensions = parse(".vscode/extensions.json", VSCODE_EXTENSIONS);
    let recommendations = object_array(&extensions, "recommendations");
    assert!(
        recommendations
            .iter()
            .any(|item| item == "rust-lang.rust-analyzer")
    );
    assert!(
        recommendations
            .iter()
            .any(|item| item == "vadimcn.vscode-lldb")
    );
}
