use super::manifests::WorkspaceManifests;
use std::path::Path;
use tempfile::tempdir;

#[test]
fn test_is_manifest_path() {
    assert!(WorkspaceManifests::is_manifest_path(Path::new(
        "Cargo.toml"
    )));
    assert!(WorkspaceManifests::is_manifest_path(Path::new(
        "crates/core/Cargo.toml"
    )));
    assert!(WorkspaceManifests::is_manifest_path(Path::new("go.mod")));
    assert!(WorkspaceManifests::is_manifest_path(Path::new(
        "package.json"
    )));
    assert!(WorkspaceManifests::is_manifest_path(Path::new(
        "pnpm-workspace.yaml"
    )));
    assert!(WorkspaceManifests::is_manifest_path(Path::new(
        "tsconfig.json"
    )));
    assert!(WorkspaceManifests::is_manifest_path(Path::new(
        "tsconfig.base.json"
    )));

    assert!(!WorkspaceManifests::is_manifest_path(Path::new(
        "src/lib.rs"
    )));
    assert!(!WorkspaceManifests::is_manifest_path(Path::new(
        "Cargo.lock"
    )));
    assert!(!WorkspaceManifests::is_manifest_path(Path::new(
        "workspace.yaml"
    )));
}

#[test]
fn test_rust_crate_discovery_and_lookup() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    let core_dir = root.join("crates/cortx-core");
    std::fs::create_dir_all(&core_dir).unwrap();
    std::fs::write(
        core_dir.join("Cargo.toml"),
        r#"
[package]
name = "cortx-core"
version = "0.1.0"
"#,
    )
    .unwrap();

    let distill_dir = root.join("crates/cortx-distill");
    std::fs::create_dir_all(&distill_dir).unwrap();
    std::fs::write(
        distill_dir.join("Cargo.toml"),
        r#"
[package]
name = "cortx-distill"
version = "0.1.0"

[lib]
name = "cortx_distill_custom"
"#,
    )
    .unwrap();

    let manifests = WorkspaceManifests::discover(root, &[]);

    assert_eq!(
        manifests.rust_crate_for_path(&core_dir.join("src/lib.rs")),
        Some("cortx_core")
    );
    assert_eq!(
        manifests.rust_crate_for_path(Path::new("crates/cortx-core/src/span.rs")),
        Some("cortx_core")
    );
    assert_eq!(
        manifests.rust_crate_for_path(&distill_dir.join("src/lib.rs")),
        Some("cortx_distill_custom")
    );
    assert_eq!(
        manifests.rust_crate_for_path(Path::new("unknown/file.rs")),
        None
    );
}

#[test]
fn test_go_module_discovery_and_filtering() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    std::fs::write(
        root.join("go.mod"),
        r#"
module github.com/acme/project // primary module

go 1.24
"#,
    )
    .unwrap();

    let nested_dir = root.join("services/auth");
    std::fs::create_dir_all(&nested_dir).unwrap();
    std::fs::write(
        nested_dir.join("go.mod"),
        r#"
module github.com/acme/project/services/auth
"#,
    )
    .unwrap();

    let manifests = WorkspaceManifests::discover(root, &[]);

    assert!(manifests.is_workspace_go_module("github.com/acme/project"));
    assert!(manifests.is_workspace_go_module("github.com/acme/project/pkg/util"));
    assert!(manifests.is_workspace_go_module("github.com/acme/project/services/auth"));
    assert!(manifests.is_workspace_go_module("./local/pkg"));

    assert!(!manifests.is_workspace_go_module("fmt"));
    assert!(!manifests.is_workspace_go_module("net/http"));
    assert!(!manifests.is_workspace_go_module("github.com/gin-gonic/gin"));

    assert_eq!(
        manifests.go_package_for_path(&root.join("pkg/util/file.go")),
        Some("github.com/acme/project/pkg/util".to_string())
    );
    assert_eq!(
        manifests.go_package_for_path(&nested_dir.join("handler.go")),
        Some("github.com/acme/project/services/auth".to_string())
    );
}

#[test]
fn test_typescript_workspaces_and_resolution() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    std::fs::write(
        root.join("package.json"),
        r#"
{
  "name": "root-pkg",
  "workspaces": [
    "packages/*",
    "!packages/ignored"
  ]
}
"#,
    )
    .unwrap();

    let shared_dir = root.join("packages/shared");
    let shared_src = shared_dir.join("src");
    std::fs::create_dir_all(&shared_src).unwrap();
    std::fs::write(
        shared_dir.join("package.json"),
        r#"
{
  "name": "@acme/shared",
  "main": "./dist/index.js"
}
"#,
    )
    .unwrap();
    std::fs::write(shared_src.join("index.ts"), "export const answer = 42;").unwrap();

    let ignored_dir = root.join("packages/ignored");
    std::fs::create_dir_all(&ignored_dir).unwrap();
    std::fs::write(
        ignored_dir.join("package.json"),
        r#"{"name": "@acme/ignored"}"#,
    )
    .unwrap();

    let manifests = WorkspaceManifests::discover(root, &[]);

    assert!(manifests.ts_packages.contains_key("@acme/shared"));
    assert!(!manifests.ts_packages.contains_key("@acme/ignored"));

    let app_dir = root.join("packages/app");
    std::fs::create_dir_all(&app_dir).unwrap();
    let app_file = app_dir.join("main.ts");

    let resolved = manifests.resolve_ts_import(&app_file, "@acme/shared");
    assert!(resolved.is_ok(), "Failed to resolve: {:?}", resolved.err());
    let target = resolved.unwrap();
    assert_eq!(
        target.canonicalize().unwrap(),
        shared_src.join("index.ts").canonicalize().unwrap()
    );
}

#[test]
fn test_pnpm_workspaces_and_dist_to_src_fallback() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    std::fs::write(
        root.join("pnpm-workspace.yaml"),
        r#"
packages:
  - 'packages/*'
"#,
    )
    .unwrap();

    let ui_dir = root.join("packages/ui");
    let ui_src = ui_dir.join("src");
    let ui_dist = ui_dir.join("dist");
    std::fs::create_dir_all(&ui_src).unwrap();
    std::fs::create_dir_all(&ui_dist).unwrap();

    std::fs::write(
        ui_dir.join("package.json"),
        r#"
{
  "name": "@acme/ui",
  "main": "./dist/index.js"
}
"#,
    )
    .unwrap();

    // Create both compiled dist/index.js AND source src/index.tsx
    std::fs::write(ui_dist.join("index.js"), "module.exports = {};").unwrap();
    std::fs::write(
        ui_src.join("index.tsx"),
        "export const Button = () => null;",
    )
    .unwrap();

    let manifests = WorkspaceManifests::discover(root, &[]);
    assert!(manifests.ts_packages.contains_key("@acme/ui"));

    let app_file = root.join("app.tsx");
    let resolved = manifests.resolve_ts_import(&app_file, "@acme/ui");
    assert!(resolved.is_ok(), "Failed to resolve: {:?}", resolved.err());
    let target = resolved.unwrap();
    // Fallback must prioritize src/index.tsx over dist/index.js
    assert_eq!(
        target.canonicalize().unwrap(),
        ui_src.join("index.tsx").canonicalize().unwrap()
    );
}
