//! Non-destructive advisory tool hook handler for agent integrations.

use crate::error::DaemonError;
use cortx_core::ReadCoverage;
use cortx_core::model::DistillMode;
use cortx_distill::{DistillBundle, DistillQuery, OverlayView, SymbolIndex, distill_with_query};
use std::path::Path;

/// Executes non-destructive post-read advisory context injection using a query and overlay view.
///
/// CRITICAL INVARIANT: The target file on disk is guaranteed strictly byte-identical
/// and untouched. Injected context is emitted strictly out-of-band.
pub fn execute_post_read_query(
    query: &DistillQuery<'_>,
    view: OverlayView<'_>,
) -> Result<DistillBundle, DaemonError> {
    if !query.file_path.is_file() {
        return Err(DaemonError::Watcher(format!(
            "target file '{}' does not exist or is not a regular file",
            query.file_path.display()
        )));
    }

    let bundle = distill_with_query(query, view)?;
    Ok(bundle)
}

/// Executes non-destructive post-read advisory context injection, returning the raw `DistillBundle`.
///
/// CRITICAL INVARIANT: The target file on disk is guaranteed strictly byte-identical
/// before and after this function executes. Injected context is emitted strictly out-of-band.
pub fn execute_post_read_bundle(
    file_path: &Path,
    index: Option<&SymbolIndex>,
) -> Result<DistillBundle, DaemonError> {
    execute_post_read_bundle_for_project(file_path, index, None, None)
}

/// Executes non-destructive post-read advisory context injection with per-project distillation options,
/// returning the raw `DistillBundle`.
///
/// CRITICAL INVARIANT: The target file on disk is guaranteed strictly byte-identical
/// before and after this function executes. Injected context is emitted strictly out-of-band.
pub fn execute_post_read_bundle_for_project(
    file_path: &Path,
    base_index: Option<&SymbolIndex>,
    overlay_index: Option<&SymbolIndex>,
    project: Option<&cortx_core::ProjectConfig>,
) -> Result<DistillBundle, DaemonError> {
    let view = OverlayView {
        base: base_index,
        overlay: overlay_index,
        shadowed_files: None,
    };
    let query = DistillQuery {
        file_path,
        mode: DistillMode::Bundle,
        coverage: ReadCoverage::PartialMacro,
        span: None,
        preloaded_source: None,
        cross_boundary: true,
        project_cfg: project,
    };
    execute_post_read_query(&query, view)
}

/// Executes non-destructive post-read advisory context injection.
///
/// CRITICAL INVARIANT: The target file on disk is guaranteed strictly byte-identical
/// before and after this function executes. Injected context is emitted strictly out-of-band.
pub fn execute_post_read_hook(
    file_path: &Path,
    index: Option<&SymbolIndex>,
) -> Result<String, DaemonError> {
    execute_post_read_hook_with_overlay(file_path, index, None)
}

/// Executes non-destructive post-read advisory context injection, querying an optional overlay
/// index before falling through to the base index.
///
/// CRITICAL INVARIANT: The target file on disk is guaranteed strictly byte-identical
/// before and after this function executes. Injected context is emitted strictly out-of-band.
pub fn execute_post_read_hook_with_overlay(
    file_path: &Path,
    base_index: Option<&SymbolIndex>,
    overlay_index: Option<&SymbolIndex>,
) -> Result<String, DaemonError> {
    execute_post_read_hook_for_project(file_path, base_index, overlay_index, None)
}

/// Executes non-destructive post-read advisory context injection with per-project distillation options.
///
/// CRITICAL INVARIANT: The target file on disk is guaranteed strictly byte-identical
/// before and after this function executes. Injected context is emitted strictly out-of-band.
pub fn execute_post_read_hook_for_project(
    file_path: &Path,
    base_index: Option<&SymbolIndex>,
    overlay_index: Option<&SymbolIndex>,
    project: Option<&cortx_core::ProjectConfig>,
) -> Result<String, DaemonError> {
    let bundle =
        execute_post_read_bundle_for_project(file_path, base_index, overlay_index, project)?;
    Ok(bundle.format_markdown())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_non_destructive_invariant() {
        let mut file = NamedTempFile::new().unwrap();
        let content = b"pub fn calculate(x: u32) -> u32 {\n    x * 2\n}\n";
        file.write_all(content).unwrap();
        file.flush().unwrap();

        let meta_before = std::fs::metadata(file.path()).unwrap();

        let advisory = execute_post_read_hook(file.path(), None).unwrap();
        assert!(advisory.contains("# Cortx Context Distillation"));
        assert!(advisory.contains("pub fn calculate"));

        let meta_after = std::fs::metadata(file.path()).unwrap();

        // Verify file is 100% byte-identical
        let disk_bytes = std::fs::read(file.path()).unwrap();
        assert_eq!(disk_bytes, content);

        // Verify mtime and inode remain strictly untouched
        assert_eq!(
            meta_before.modified().unwrap(),
            meta_after.modified().unwrap()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(meta_before.ino(), meta_after.ino());
        }
    }

    #[test]
    fn test_non_destructive_invariant_tsx() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("Dashboard.tsx");
        let content = b"export interface Props { id: string; }\nexport function Dashboard(props: Props) {\n    return <div>{props.id}</div>;\n}\n";
        std::fs::write(&file_path, content).unwrap();

        let meta_before = std::fs::metadata(&file_path).unwrap();

        let advisory = execute_post_read_hook(&file_path, None).unwrap();
        assert!(advisory.contains("# Cortx Context Distillation"));
        assert!(advisory.contains("export interface Props"));
        assert!(advisory.contains("```typescript"));

        let meta_after = std::fs::metadata(&file_path).unwrap();

        // Critical invariant: file bytes on disk MUST be strictly identical
        let disk_bytes = std::fs::read(&file_path).unwrap();
        assert_eq!(disk_bytes, content);

        // Verify mtime and inode remain strictly untouched
        assert_eq!(
            meta_before.modified().unwrap(),
            meta_after.modified().unwrap()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(meta_before.ino(), meta_after.ino());
        }
    }

    #[test]
    fn test_non_destructive_invariant_go() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("service.go");
        let content = b"package service\n\ntype User struct {\n    ID string `json:\"id\"`\n}\n\nfunc (u *User) GetID() string {\n    return u.ID\n}\n";
        std::fs::write(&file_path, content).unwrap();

        let meta_before = std::fs::metadata(&file_path).unwrap();

        let advisory = execute_post_read_hook(&file_path, None).unwrap();
        assert!(advisory.contains("# Cortx Context Distillation"));
        assert!(advisory.contains("type User struct"));
        assert!(advisory.contains("func (u *User) GetID() string;"));
        assert!(advisory.contains("```go"));

        let meta_after = std::fs::metadata(&file_path).unwrap();

        // Critical invariant: file bytes on disk MUST remain 100% strictly identical
        let disk_bytes = std::fs::read(&file_path).unwrap();
        assert_eq!(disk_bytes, content);

        // Verify mtime and inode remain strictly untouched
        assert_eq!(
            meta_before.modified().unwrap(),
            meta_after.modified().unwrap()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(meta_before.ino(), meta_after.ino());
        }
    }

    #[test]
    fn test_execute_post_read_query_with_preloaded_source() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("lib.rs");
        let disk_content = b"pub struct DiskContent;\n";
        std::fs::write(&file_path, disk_content).unwrap();

        let meta_before = std::fs::metadata(&file_path).unwrap();

        let preloaded = "pub struct PreloadedContent;\npub fn foo() {}\n";
        let query = DistillQuery {
            file_path: &file_path,
            mode: DistillMode::Bundle,
            coverage: ReadCoverage::PartialMacro,
            span: None,
            preloaded_source: Some(preloaded),
            cross_boundary: false,
            project_cfg: None,
        };

        let bundle = execute_post_read_query(&query, OverlayView::empty()).unwrap();
        assert!(bundle.skeleton.contains("PreloadedContent"));
        assert!(!bundle.skeleton.contains("DiskContent"));

        let meta_after = std::fs::metadata(&file_path).unwrap();

        // Disk remains strictly untouched
        let disk_bytes = std::fs::read(&file_path).unwrap();
        assert_eq!(disk_bytes, disk_content);
        assert_eq!(
            meta_before.modified().unwrap(),
            meta_after.modified().unwrap()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(meta_before.ino(), meta_after.ino());
        }
    }
}
