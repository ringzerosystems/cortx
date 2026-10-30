//! Polyglot workspace manifest discovery and module resolution.
//!
//! Scans Cargo.toml, go.mod, package.json, pnpm-workspace.yaml, and tsconfig.json
//! to provide fast, zero-compiler crate, module, and TypeScript path resolution.

#![forbid(unsafe_code)]

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use oxc_resolver::{AliasValue, ResolveError, ResolveOptions, Resolver, TsconfigDiscovery};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Polyglot workspace manifests and module resolver.
#[derive(Debug)]
pub struct WorkspaceManifests {
    /// Canonical workspace root directory.
    pub workspace_root: PathBuf,
    /// Discovered Rust crates: `(crate_root_dir, normalized_crate_name)`.
    /// Sorted by directory path length descending so deeper nested crates match first.
    pub rust_crates: Vec<(PathBuf, String)>,
    /// Discovered Go modules: `(module_dir, module_path)`.
    /// Sorted by directory path length descending.
    pub go_modules: Vec<(PathBuf, String)>,
    /// Set of internal Go module import path prefixes.
    pub go_module_prefixes: HashSet<String>,
    /// Discovered TypeScript / JS workspace packages: `package_name -> package_dir`.
    pub ts_packages: HashMap<String, PathBuf>,
    /// Configured shared TypeScript resolver.
    pub resolver: Resolver,
}

impl Default for WorkspaceManifests {
    fn default() -> Self {
        Self::discover(Path::new("."), &[])
    }
}

impl WorkspaceManifests {
    /// Discovers all manifests in the workspace root, ignoring specified directory names.
    pub fn discover(workspace_root: &Path, ignored_dirs: &[String]) -> Self {
        let root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());

        let (root_pkg_name, mut patterns) = parse_root_package_json(&root.join("package.json"));
        patterns.extend(parse_pnpm_workspace_packages(
            &root.join("pnpm-workspace.yaml"),
        ));

        let (inc_glob, exc_glob) = build_workspace_globsets(&patterns);

        let mut discovered = DiscoveredManifests::default();

        if let Some(pkg_name) = root_pkg_name {
            discovered.ts_packages.insert(pkg_name, root.clone());
        }

        let scanner = ManifestScanner {
            root: &root,
            ignored_dirs,
            inc_glob: inc_glob.as_ref(),
            exc_glob: exc_glob.as_ref(),
        };
        scanner.scan(&mut discovered);

        discovered
            .rust_crates
            .sort_by_key(|a| std::cmp::Reverse(a.0.as_os_str().len()));
        discovered
            .go_modules
            .sort_by_key(|a| std::cmp::Reverse(a.0.as_os_str().len()));

        let resolver = build_oxc_resolver(&root, &discovered.ts_packages);

        Self {
            workspace_root: root,
            rust_crates: discovered.rust_crates,
            go_modules: discovered.go_modules,
            go_module_prefixes: discovered.go_module_prefixes,
            ts_packages: discovered.ts_packages,
            resolver,
        }
    }

    /// Returns true if the given path points to a known manifest or workspace configuration file.
    pub fn is_manifest_path(path: &Path) -> bool {
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            return false;
        };
        matches!(
            file_name,
            "Cargo.toml" | "go.mod" | "package.json" | "pnpm-workspace.yaml" | "tsconfig.json"
        ) || (file_name.starts_with("tsconfig.") && file_name.ends_with(".json"))
    }

    /// Resolves the normalized Rust crate identifier for a source file path.
    pub fn rust_crate_for_path(&self, file_path: &Path) -> Option<&str> {
        let clean = file_path.strip_prefix("./").unwrap_or(file_path);
        for (crate_dir, crate_name) in &self.rust_crates {
            if clean.starts_with(crate_dir) {
                return Some(crate_name.as_str());
            }
            if clean.is_relative() {
                if let Ok(rel_crate) = crate_dir.strip_prefix(&self.workspace_root) {
                    if clean.starts_with(rel_crate) {
                        return Some(crate_name.as_str());
                    }
                }
                let abs = self.workspace_root.join(clean);
                if abs.starts_with(crate_dir) {
                    return Some(crate_name.as_str());
                }
            }
        }
        None
    }

    /// Returns true if the given Go import path belongs to an internal workspace module.
    pub fn is_workspace_go_module(&self, import_path: &str) -> bool {
        let trimmed = import_path.trim().trim_matches('"');
        if trimmed.starts_with("./") || trimmed.starts_with("../") {
            return true;
        }
        for prefix in &self.go_module_prefixes {
            if trimmed == prefix || trimmed.starts_with(&format!("{prefix}/")) {
                return true;
            }
        }
        false
    }

    /// Resolves the Go module directory and module import path owning the given file.
    pub fn go_module_for_path(&self, file_path: &Path) -> Option<(&Path, &str)> {
        let clean = file_path.strip_prefix("./").unwrap_or(file_path);
        for (mod_dir, mod_path) in &self.go_modules {
            if clean.starts_with(mod_dir) {
                return Some((mod_dir.as_path(), mod_path.as_str()));
            }
            if clean.is_relative() {
                if let Ok(rel_mod) = mod_dir.strip_prefix(&self.workspace_root) {
                    if clean.starts_with(rel_mod) {
                        return Some((mod_dir.as_path(), mod_path.as_str()));
                    }
                }
                let abs = self.workspace_root.join(clean);
                if abs.starts_with(mod_dir) {
                    return Some((mod_dir.as_path(), mod_path.as_str()));
                }
            }
        }
        None
    }

    /// Resolves the full package import path for a Go source file.
    pub fn go_package_for_path(&self, file_path: &Path) -> Option<String> {
        let (mod_dir, mod_path) = self.go_module_for_path(file_path)?;
        let parent = if file_path.is_dir() {
            file_path
        } else {
            file_path.parent()?
        };
        let clean_parent = parent.strip_prefix("./").unwrap_or(parent);
        let abs_parent;
        let effective_parent = if clean_parent.is_absolute() {
            clean_parent
        } else {
            abs_parent = self.workspace_root.join(clean_parent);
            &abs_parent
        };
        let rel = effective_parent.strip_prefix(mod_dir).ok()?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let rel_trimmed = rel_str.trim_matches('/');
        if rel_trimmed.is_empty() {
            Some(mod_path.to_string())
        } else {
            Some(format!("{mod_path}/{rel_trimmed}"))
        }
    }

    /// Resolves a TypeScript/JavaScript import specifier to its target file path on disk.
    pub fn resolve_ts_import(
        &self,
        containing_file: &Path,
        specifier: &str,
    ) -> Result<PathBuf, ResolveError> {
        let base_dir = if containing_file.is_dir() {
            containing_file
        } else if let Some(parent) = containing_file.parent() {
            if parent.as_os_str().is_empty() {
                &self.workspace_root
            } else {
                parent
            }
        } else {
            &self.workspace_root
        };

        match self.resolver.resolve(base_dir, specifier) {
            Ok(resolution) => {
                let path = resolution.into_path_buf();
                if let Some(src) = try_dist_to_src_fallback(&path) {
                    Ok(src)
                } else {
                    Ok(path)
                }
            }
            Err(err) => {
                if let Some(fallback) = self.try_unbuilt_ts_fallback(base_dir, specifier) {
                    Ok(fallback)
                } else {
                    Err(err)
                }
            }
        }
    }

    /// Attempts to resolve an unbuilt workspace package import to its TypeScript source file.
    fn try_unbuilt_ts_fallback(&self, base_dir: &Path, specifier: &str) -> Option<PathBuf> {
        if specifier.starts_with("./") || specifier.starts_with("../") {
            let direct = base_dir.join(specifier);
            return try_dist_to_src_fallback(&direct);
        }

        let mut best: Option<(&String, &PathBuf)> = None;
        for (pkg_name, pkg_dir) in &self.ts_packages {
            if (specifier == pkg_name || specifier.starts_with(&format!("{pkg_name}/")))
                && best.is_none_or(|(b_name, _)| pkg_name.len() > b_name.len())
            {
                best = Some((pkg_name, pkg_dir));
            }
        }

        let (pkg_name, pkg_dir) = best?;
        let subpath = if specifier == pkg_name {
            ""
        } else {
            &specifier[pkg_name.len() + 1..]
        };

        find_ts_package_entry(pkg_dir, subpath)
    }
}

fn find_ts_package_entry(pkg_dir: &Path, subpath: &str) -> Option<PathBuf> {
    if subpath.is_empty() {
        let candidates = [
            pkg_dir.join("src/index.ts"),
            pkg_dir.join("src/index.tsx"),
            pkg_dir.join("src/main.ts"),
            pkg_dir.join("src/lib.ts"),
            pkg_dir.join("index.ts"),
            pkg_dir.join("index.tsx"),
        ];
        for cand in candidates {
            if cand.is_file() {
                return Some(cand);
            }
        }
        if let Ok(content) = std::fs::read_to_string(pkg_dir.join("package.json")) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(main) = val.get("main").and_then(|m| m.as_str()) {
                    if let Some(cand) = try_dist_to_src_fallback(&pkg_dir.join(main)) {
                        return Some(cand);
                    }
                }
            }
        }
    } else {
        let candidates = [
            pkg_dir.join("src").join(format!("{subpath}.ts")),
            pkg_dir.join("src").join(format!("{subpath}.tsx")),
            pkg_dir.join("src").join(subpath).join("index.ts"),
            pkg_dir.join("src").join(subpath).join("index.tsx"),
            pkg_dir.join(format!("{subpath}.ts")),
            pkg_dir.join(format!("{subpath}.tsx")),
        ];
        for cand in candidates {
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Fallback replacing `/dist/` and `.js`/`.mjs`/`.d.ts` with `/src/` and `.ts`/`.tsx`.
fn try_dist_to_src_fallback(path: &Path) -> Option<PathBuf> {
    let components: Vec<_> = path.components().collect();
    let dist_idx = components.iter().position(|c| c.as_os_str() == "dist")?;

    let mut candidate = PathBuf::new();
    for (i, c) in components.iter().enumerate() {
        if i == dist_idx {
            candidate.push("src");
        } else {
            candidate.push(c.as_os_str());
        }
    }

    let file_name = candidate.file_name()?.to_string_lossy().to_string();
    let stem = if let Some(base) = file_name.strip_suffix(".d.ts") {
        base
    } else if let Some(base) = file_name.strip_suffix(".js") {
        base
    } else if let Some(base) = file_name.strip_suffix(".mjs") {
        base
    } else if let Some(base) = file_name.strip_suffix(".cjs") {
        base
    } else {
        if candidate.is_file() {
            return Some(candidate);
        }
        return None;
    };

    let parent = candidate.parent()?;
    let ts_cand = parent.join(format!("{stem}.ts"));
    if ts_cand.is_file() {
        return Some(ts_cand);
    }
    let tsx_cand = parent.join(format!("{stem}.tsx"));
    if tsx_cand.is_file() {
        return Some(tsx_cand);
    }
    None
}

#[derive(Deserialize)]
struct CargoManifest {
    package: Option<CargoPackage>,
    lib: Option<CargoLib>,
}

#[derive(Deserialize)]
struct CargoPackage {
    name: String,
}

#[derive(Deserialize)]
struct CargoLib {
    name: Option<String>,
}

fn parse_cargo_crate(content: &str) -> Option<String> {
    let manifest: CargoManifest = toml::from_str(content).ok()?;
    let lib_name = manifest.lib.and_then(|l| l.name);
    let pkg_name = manifest.package.map(|p| p.name);
    lib_name.or(pkg_name).map(|name| name.replace('-', "_"))
}

fn parse_go_module_path(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("module") {
            let mod_part = rest.split("//").next().unwrap_or("").trim();
            let clean = mod_part.trim_matches('"').trim_matches('\'');
            if !clean.is_empty() {
                return Some(clean.to_string());
            }
        }
    }
    None
}

#[derive(Deserialize)]
struct PackageJsonInfo {
    name: Option<String>,
    workspaces: Option<WorkspacesValue>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WorkspacesValue {
    List(Vec<String>),
    Object { packages: Option<Vec<String>> },
}

fn parse_root_package_json(path: &Path) -> (Option<String>, Vec<String>) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return (None, Vec::new());
    };
    let Ok(info) = serde_json::from_str::<PackageJsonInfo>(&content) else {
        return (None, Vec::new());
    };
    let mut patterns = Vec::new();
    if let Some(ws) = info.workspaces {
        match ws {
            WorkspacesValue::List(list) => patterns.extend(list),
            WorkspacesValue::Object { packages } => {
                if let Some(pkgs) = packages {
                    patterns.extend(pkgs);
                }
            }
        }
    }
    (info.name, patterns)
}

fn parse_pnpm_workspace_packages(path: &Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut packages = Vec::new();
    let mut in_packages = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if trimmed == "packages:" || trimmed.starts_with("packages:") {
            in_packages = true;
            continue;
        }
        if in_packages {
            if line.starts_with(' ') || line.starts_with('\t') {
                if let Some(rest) = trimmed.strip_prefix("- ") {
                    let pat = rest.trim().trim_matches(|c| c == '\'' || c == '"');
                    if !pat.is_empty() {
                        packages.push(pat.to_string());
                    }
                }
            } else {
                break;
            }
        }
    }
    packages
}

fn build_workspace_globsets(patterns: &[String]) -> (Option<GlobSet>, Option<GlobSet>) {
    if patterns.is_empty() {
        return (None, None);
    }
    let mut inc_builder = GlobSetBuilder::new();
    let mut exc_builder = GlobSetBuilder::new();
    let mut inc_count = 0;
    let mut exc_count = 0;

    for pat in patterns {
        let trimmed = pat.trim();
        if let Some(rest) = trimmed.strip_prefix('!') {
            let clean = rest.trim_end_matches('/');
            if let Ok(glob) = GlobBuilder::new(clean).literal_separator(true).build() {
                exc_builder.add(glob);
                exc_count += 1;
            }
        } else {
            let clean = trimmed.trim_end_matches('/');
            if let Ok(glob) = GlobBuilder::new(clean).literal_separator(true).build() {
                inc_builder.add(glob);
                inc_count += 1;
            }
        }
    }

    let inc = if inc_count > 0 {
        inc_builder.build().ok()
    } else {
        None
    };
    let exc = if exc_count > 0 {
        exc_builder.build().ok()
    } else {
        None
    };
    (inc, exc)
}

fn is_ignored_dir(name: &str, ignored_dirs: &[String]) -> bool {
    if name.starts_with('.') && name != "." {
        return true;
    }
    matches!(
        name,
        "node_modules" | "target" | "dist" | ".turbo" | ".next"
    ) || ignored_dirs.iter().any(|ig| ig == name)
}

fn build_oxc_resolver(root: &Path, ts_packages: &HashMap<String, PathBuf>) -> Resolver {
    let aliases = ts_packages
        .iter()
        .map(|(pkg_name, pkg_dir)| {
            (
                pkg_name.clone(),
                vec![AliasValue::from(pkg_dir.to_string_lossy().to_string())],
            )
        })
        .collect();

    let options = ResolveOptions {
        tsconfig: Some(TsconfigDiscovery::Auto),
        extensions: vec![
            ".ts".into(),
            ".tsx".into(),
            ".js".into(),
            ".jsx".into(),
            ".mjs".into(),
            ".cjs".into(),
            ".json".into(),
        ],
        extension_alias: vec![
            (
                ".js".into(),
                vec![".ts".into(), ".tsx".into(), ".js".into()],
            ),
            (".mjs".into(), vec![".mts".into(), ".mjs".into()]),
            (".cjs".into(), vec![".cts".into(), ".cjs".into()]),
        ],
        condition_names: vec![
            "types".into(),
            "import".into(),
            "require".into(),
            "default".into(),
        ],
        roots: vec![root.to_path_buf()],
        alias: aliases,
        ..Default::default()
    };

    Resolver::new(options)
}

#[derive(Default)]
struct DiscoveredManifests {
    rust_crates: Vec<(PathBuf, String)>,
    go_modules: Vec<(PathBuf, String)>,
    go_module_prefixes: HashSet<String>,
    ts_packages: HashMap<String, PathBuf>,
}

struct ManifestScanner<'a> {
    root: &'a Path,
    ignored_dirs: &'a [String],
    inc_glob: Option<&'a GlobSet>,
    exc_glob: Option<&'a GlobSet>,
}

impl ManifestScanner<'_> {
    fn scan(&self, out: &mut DiscoveredManifests) {
        let walker = WalkDir::new(self.root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                if entry.depth() == 0 {
                    return true;
                }
                if entry.file_type().is_dir() {
                    let name = entry.file_name().to_string_lossy();
                    !is_ignored_dir(&name, self.ignored_dirs)
                } else {
                    true
                }
            });

        for entry in walker.filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }
            let file_name = entry.file_name().to_string_lossy();
            let path = entry.path();
            let Some(dir) = path.parent() else {
                continue;
            };

            if file_name == "Cargo.toml" {
                if let Ok(content) = std::fs::read_to_string(path) {
                    if let Some(crate_name) = parse_cargo_crate(&content) {
                        out.rust_crates.push((dir.to_path_buf(), crate_name));
                    }
                }
            } else if file_name == "go.mod" {
                if let Ok(content) = std::fs::read_to_string(path) {
                    if let Some(mod_path) = parse_go_module_path(&content) {
                        out.go_modules.push((dir.to_path_buf(), mod_path.clone()));
                        out.go_module_prefixes.insert(mod_path);
                    }
                }
            } else if file_name == "package.json" && dir != self.root {
                let rel = dir.strip_prefix(self.root).unwrap_or(dir);
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                let is_match = if let Some(inc) = self.inc_glob {
                    inc.is_match(&rel_str)
                        && self.exc_glob.is_none_or(|exc| !exc.is_match(&rel_str))
                } else {
                    true
                };

                if is_match {
                    if let Ok(content) = std::fs::read_to_string(path) {
                        if let Ok(info) = serde_json::from_str::<PackageJsonInfo>(&content) {
                            if let Some(name) = info.name {
                                out.ts_packages.insert(name, dir.to_path_buf());
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
