use super::*;
use tempfile::NamedTempFile;

#[test]
fn test_default_config() {
    let cfg = CortxConfig::default();
    assert_eq!(cfg.general.codebase_scale, "massive");
    assert_eq!(cfg.distill.max_call_depth, 2);
    assert!(cfg.distill.omit_tests);
    assert_eq!(cfg.daemon.socket_path, None);
    assert_eq!(cfg.daemon.session_ttl_secs, DEFAULT_SESSION_TTL_SECS);
    assert_eq!(cfg.daemon.session_ttl_secs, 28800);
}

#[test]
fn test_daemon_config_socket_override() {
    use std::io::Write;
    let mut file = NamedTempFile::new().unwrap();
    writeln!(
        file,
        r#"
[daemon]
socket_path = "/tmp/custom-test.sock"
"#
    )
    .unwrap();

    let cfg = CortxConfig::load(file.path()).unwrap();
    assert_eq!(
        cfg.daemon.socket_path,
        Some("/tmp/custom-test.sock".to_string())
    );
}

#[test]
fn test_load_config_file() {
    use std::io::Write;
    let mut file = NamedTempFile::new().unwrap();
    writeln!(
        file,
        r#"
[general]
codebase_scale = "small"
default_format = "json"

[distill]
max_call_depth = 5
"#
    )
    .unwrap();

    let cfg = CortxConfig::load(file.path()).unwrap();
    assert_eq!(cfg.general.codebase_scale, "small");
    assert_eq!(cfg.general.default_format, OutputFormat::Json);
    assert_eq!(cfg.distill.max_call_depth, 5);
}

#[test]
fn test_load_polyglot_config_file() {
    use std::io::Write;
    let mut file = NamedTempFile::new().unwrap();
    writeln!(
        file,
        r#"
[general]
codebase_scale = "massive"

[typescript]
enabled = true
tsconfig_path = "frontend/tsconfig.json"
extensions = ["ts", "tsx", "js", "jsx"]
strip_jsx = true

[cross_boundary]
rust_models = "crates/shared-types/src"
ts_types = "frontend/src/types"
strict_casing_check = true
"#
    )
    .unwrap();

    let cfg = CortxConfig::load(file.path()).unwrap();
    assert!(cfg.typescript.enabled);
    assert_eq!(cfg.typescript.tsconfig_path, "frontend/tsconfig.json");
    assert!(cfg.typescript.strip_jsx);
    assert_eq!(cfg.cross_boundary.rust_models, "crates/shared-types/src");
    assert_eq!(cfg.cross_boundary.ts_types, "frontend/src/types");
    assert!(cfg.cross_boundary.strict_casing_check);
}

#[test]
fn test_load_go_config() {
    use std::io::Write;
    let mut file = NamedTempFile::new().unwrap();
    writeln!(
        file,
        r#"
[general]
codebase_scale = "massive"

[go]
enabled = true
module_root = "backend"
extensions = ["go"]
strip_function_bodies = true
index_implicit_interfaces = true
include_private_types = false

[cross_boundary]
rust_models = "crates/core/src/types"
ts_types = "frontend/src/types"
go_models = "backend/pkg/models"
strict_casing_check = true
strict_omitempty_check = true
strict_nil_slice_check = true
"#
    )
    .unwrap();

    let cfg = CortxConfig::load(file.path()).unwrap();
    assert!(cfg.go.enabled);
    assert_eq!(cfg.go.module_root, "backend");
    assert_eq!(cfg.go.extensions, vec!["go"]);
    assert!(cfg.go.strip_function_bodies);
    assert!(cfg.go.index_implicit_interfaces);
    assert!(!cfg.go.include_private_types);
    assert_eq!(cfg.cross_boundary.go_models, "backend/pkg/models");
    assert!(cfg.cross_boundary.strict_omitempty_check);
    assert!(cfg.cross_boundary.strict_nil_slice_check);
}

#[test]
fn test_daemon_config_multi_tenant_fields() {
    use std::io::Write;
    let mut file = NamedTempFile::new().unwrap();
    writeln!(
        file,
        r#"
[daemon]
system_daemon = true
global_socket = "/tmp/global.sock"
idle_ttl_secs = 3600
session_ttl_secs = 14400
enable_file_watcher = false
log_dir = "/var/log/cortx"
workspace_logging = true
"#
    )
    .unwrap();

    let cfg = CortxConfig::load(file.path()).unwrap();
    assert!(cfg.daemon.system_daemon);
    assert_eq!(
        cfg.daemon.global_socket,
        Some("/tmp/global.sock".to_string())
    );
    assert_eq!(cfg.daemon.idle_ttl_secs, 3600);
    assert_eq!(cfg.daemon.session_ttl_secs, 14400);
    assert!(!cfg.daemon.enable_file_watcher);
    assert_eq!(cfg.daemon.log_dir, Some("/var/log/cortx".to_string()));
    assert!(cfg.daemon.workspace_logging);
}

#[test]
fn test_daemon_config_session_ttl_default_and_custom() {
    use std::io::Write;
    let mut file = NamedTempFile::new().unwrap();
    writeln!(
        file,
        r#"
[daemon]
idle_ttl_secs = 1800
"#
    )
    .unwrap();

    let cfg = CortxConfig::load(file.path()).unwrap();
    assert_eq!(cfg.daemon.idle_ttl_secs, 1800);
    assert_eq!(cfg.daemon.session_ttl_secs, 28800);

    let mut custom_file = NamedTempFile::new().unwrap();
    writeln!(
        custom_file,
        r#"
[daemon]
session_ttl_secs = 3600
"#
    )
    .unwrap();

    let custom_cfg = CortxConfig::load(custom_file.path()).unwrap();
    assert_eq!(custom_cfg.daemon.session_ttl_secs, 3600);
}

#[test]
fn test_config_merge() {
    let mut base = CortxConfig::default();
    let mut overlay = CortxConfig::default();
    overlay.general.codebase_scale = "small".to_string();
    overlay.daemon.idle_ttl_secs = 1800;
    overlay.daemon.session_ttl_secs = 3600;

    base.merge(overlay);
    assert_eq!(base.general.codebase_scale, "small");
    assert_eq!(base.daemon.idle_ttl_secs, 1800);
    assert_eq!(base.daemon.session_ttl_secs, 3600);
}

#[test]
fn test_resolve_global_config_path() {
    let path = resolve_global_config_path();
    assert!(path.to_str().unwrap().contains("cortx.toml"));
}

#[test]
fn test_project_config_deserialization_and_lookup() {
    use std::io::Write;
    let mut file = NamedTempFile::new().unwrap();
    writeln!(
        file,
        r#"
[daemon]
system_daemon = true
lazy_indexing = true

[acme-web]
qualified_path = "/workspace/projects/acme-web"
has_worktrees = true
has_custom_worktree_path = "/workspace/projects/acme-web/.claude/worktrees"
lang = "rust,typescript"
codebase_scale = "massive"
watch_paths = ["crates", "frontend/src"]
ignored_dirs = [".claude", ".cursor"]
max_call_depth = 3
strip_comments = true
include_private_types = false
custom_ignored_dirs = ["dist"]

[acme-web.typescript]
enabled = true
tsconfig_path = "frontend/tsconfig.json"

[acme-web.cross_boundary]
rust_models = "crates/shared-types/src"
ts_types = "frontend/src/types"

[payment-gateway]
qualified_path = "/workspace/projects/payment-gateway"
has_worktrees = false
lang = ["go"]
codebase_scale = "medium"
watch_paths = ["backend", "pkg"]
ignored_dirs = ["vendor"]

[payment-gateway.go]
enabled = true
module_root = "backend"
"#
    )
    .unwrap();

    let (cfg, loaded_path) = CortxConfig::load_daemon_config(Some(file.path())).unwrap();
    assert!(loaded_path.is_some());
    assert!(cfg.daemon.lazy_indexing);
    assert_eq!(cfg.projects.len(), 2);

    let acme = cfg.projects.get("acme-web").unwrap();
    assert!(acme.has_worktrees);
    assert_eq!(acme.lang, vec!["rust", "typescript"]);
    assert_eq!(acme.distill.max_call_depth, 3);
    assert!(acme.distill.strip_comments);
    assert!(!acme.distill.include_private_types);
    assert_eq!(acme.distill.custom_ignored_dirs, vec!["dist"]);
    assert!(acme.is_extension_enabled("rs"));
    assert!(acme.is_extension_enabled("tsx"));
    assert!(!acme.is_extension_enabled("go"));
    assert!(acme.is_relative_path_ignored(Path::new(".claude/logs.txt")));
    assert!(acme.is_relative_path_ignored(Path::new("target/debug/main")));
    assert!(acme.is_relative_path_ignored(Path::new("dist/bundle.js")));
    assert!(!acme.is_relative_path_ignored(Path::new("crates/core/src/lib.rs")));
    assert!(acme.is_dir_in_watch_paths(Path::new("")));
    assert!(acme.is_dir_in_watch_paths(Path::new("crates/core")));
    assert!(acme.is_dir_in_watch_paths(Path::new("frontend")));
    assert!(!acme.is_dir_in_watch_paths(Path::new("scripts")));
    assert!(acme.is_file_in_watch_paths(Path::new("crates/core/src/lib.rs")));
    assert!(acme.is_file_in_watch_paths(Path::new("frontend/src/App.tsx")));
    assert!(!acme.is_file_in_watch_paths(Path::new("frontend/public/index.html")));

    let pg = cfg.projects.get("payment-gateway").unwrap();
    assert_eq!(pg.lang, vec!["go"]);
    assert!(pg.is_extension_enabled("go"));
    assert!(!pg.is_extension_enabled("rs"));

    let (matched_name, _) = cfg
        .find_project_for_path(Path::new(
            "/workspace/projects/acme-web/.claude/worktrees/wt-1/src/lib.rs",
        ))
        .unwrap();
    assert_eq!(matched_name, "acme-web");
}

#[test]
fn test_longest_prefix_project_matching_and_system_config_error() {
    use std::io::Write;
    let mut cfg = CortxConfig::default();
    let root_proj = ProjectConfig {
        qualified_path: PathBuf::from("/workspace/monorepo"),
        ..Default::default()
    };
    let sub_proj = ProjectConfig {
        qualified_path: PathBuf::from("/workspace/monorepo/services/payments"),
        custom_worktree_path: Some(PathBuf::from("./.claude/worktrees")),
        watch_paths: vec!["./src".to_string()],
        ignored_dirs: vec!["./generated/proto".to_string()],
        ..Default::default()
    };
    assert_eq!(
        sub_proj.relative_custom_worktree_path(Path::new("/workspace/monorepo/services/payments")),
        Some(Path::new(".claude/worktrees"))
    );
    assert!(sub_proj.is_relative_path_ignored(Path::new("generated/proto/api.rs")));
    assert!(sub_proj.is_relative_path_ignored(Path::new("./generated/proto/api.rs")));
    assert!(sub_proj.is_dir_in_watch_paths(Path::new("src/handlers")));
    assert!(sub_proj.is_file_in_watch_paths(Path::new("src/main.rs")));
    assert!(!sub_proj.is_file_in_watch_paths(Path::new("scripts/deploy.rs")));

    cfg.projects.insert("z-root".to_string(), root_proj.clone());
    cfg.projects.insert("a-payments".to_string(), sub_proj);
    assert_eq!(
        cfg.find_project_for_path(Path::new("/workspace/monorepo/services/payments/main.rs"))
            .unwrap()
            .0,
        "a-payments"
    );
    assert_eq!(
        cfg.find_project_for_path(Path::new("/workspace/monorepo/crates/core/lib.rs"))
            .unwrap()
            .0,
        "z-root"
    );

    let mut bad_sys = NamedTempFile::new().unwrap();
    writeln!(bad_sys, "[daemon\ninvalid_toml = ").unwrap();
    assert!(
        CortxConfig::load_daemon_config_with_system(None, Some(bad_sys.path().to_path_buf()))
            .is_err()
    );
}

#[test]
fn test_load_or_default_custom_and_system() {
    let file = NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "[general]\ncodebase_scale = \"small\"\n").unwrap();

    let (cfg, prov) = CortxConfig::load_or_default(Some(file.path())).unwrap();
    assert_eq!(cfg.general.codebase_scale, "small");
    assert_eq!(
        prov.custom_path.as_deref(),
        Some(std::fs::canonicalize(file.path()).unwrap().as_path())
    );
    assert_eq!(prov.system_path, None);
    assert_eq!(
        prov.primary_path(),
        Some(std::fs::canonicalize(file.path()).unwrap().as_path())
    );

    let default_path = resolve_default_config_path();
    assert!(default_path.ends_with("cortx.toml"));
}

#[test]
fn test_template_config_file_validity() {
    let template_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../configs/cortx.example.toml");
    let (cfg, loaded_path) =
        CortxConfig::load_daemon_config(Some(&template_path)).expect("failed to load template");
    assert!(loaded_path.is_some());
    assert!(cfg.daemon.system_daemon);
    assert!(cfg.daemon.lazy_indexing);
    assert_eq!(cfg.daemon.idle_ttl_secs, 7200);
    assert!(cfg.daemon.enable_file_watcher);
    assert!(cfg.daemon.workspace_logging);

    assert_eq!(cfg.projects.len(), 4);
    assert!(cfg.projects.contains_key("acme-fullstack"));
    assert!(cfg.projects.contains_key("payment-service"));
    assert!(cfg.projects.contains_key("storage-engine"));
    assert!(cfg.projects.contains_key("web-dashboard"));

    let acme = cfg.projects.get("acme-fullstack").unwrap();
    assert!(acme.has_worktrees);
    assert_eq!(acme.lang, vec!["rust", "typescript"]);
    assert!(acme.distill.omit_tests);
    assert!(acme.omit_tests);
    assert!(acme.typescript.enabled);
    assert!(acme.typescript.strip_jsx);
    assert!(acme.typescript.strip_hook_bodies);
    assert!(acme.cross_boundary.strict_casing_check);

    let payment = cfg.projects.get("payment-service").unwrap();
    assert!(!payment.has_worktrees);
    assert_eq!(payment.lang, vec!["go"]);
    assert!(payment.go.enabled);
    assert!(payment.go.index_implicit_interfaces);

    let storage = cfg.projects.get("storage-engine").unwrap();
    assert!(!storage.has_worktrees);
    assert_eq!(storage.lang, vec!["rust"]);
    assert_eq!(storage.distill.max_call_depth, 3);
    assert!(storage.distill.omit_tests);

    let web = cfg.projects.get("web-dashboard").unwrap();
    assert!(!web.has_worktrees);
    assert_eq!(web.lang, vec!["typescript"]);
    assert_eq!(web.codebase_scale, "small");
    assert!(web.typescript.enabled);
}

#[test]
fn test_distill_config_omit_tests_defaults_and_override() {
    use std::io::Write;

    let default_distill = DistillConfig::default();
    assert!(default_distill.omit_tests);

    let default_proj = ProjectConfig::default();
    assert!(default_proj.omit_tests);
    assert!(default_proj.distill.omit_tests);

    let toml_omitted = r#"
[distill]
max_call_depth = 4

[proj1]
qualified_path = "/workspace/proj1"
"#;
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "{}", toml_omitted).unwrap();
    let (cfg, _) = CortxConfig::load_daemon_config(Some(file.path())).unwrap();
    assert!(cfg.distill.omit_tests);
    let p = cfg.projects.get("proj1").unwrap();
    assert!(p.omit_tests);
    assert!(p.distill.omit_tests);

    let toml_explicit_false = r#"
[distill]
omit_tests = false

[proj_false]
qualified_path = "/workspace/proj_false"
omit_tests = false

[proj_distill_false]
qualified_path = "/workspace/proj_distill_false"
[proj_distill_false.distill]
omit_tests = false
"#;
    let mut file2 = NamedTempFile::new().unwrap();
    writeln!(file2, "{}", toml_explicit_false).unwrap();
    let (cfg2, _) = CortxConfig::load_daemon_config(Some(file2.path())).unwrap();
    assert!(!cfg2.distill.omit_tests);

    let pf = cfg2.projects.get("proj_false").unwrap();
    assert!(!pf.omit_tests);
    assert!(!pf.distill.omit_tests);

    let psf = cfg2.projects.get("proj_distill_false").unwrap();
    assert!(!psf.omit_tests);
    assert!(!psf.distill.omit_tests);
}
