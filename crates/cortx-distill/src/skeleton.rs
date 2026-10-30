//! AST interface skeleton stripper using syn::visit_mut::VisitMut.

use crate::error::DistillError;
use std::path::Path;
use syn::visit_mut::VisitMut;

/// Determines if an attribute matches standard test annotations:
/// `#[cfg(test)]`, `#[test]`, `#[tokio::test]`, `#[async_std::test]`, `#[kani::proof]`, etc.
pub fn is_test_attribute(attr: &syn::Attribute) -> bool {
    let path = attr.path();
    if path.is_ident("test") || path.is_ident("rstest") || path.is_ident("test_case") {
        return true;
    }

    let segs: Vec<&syn::Ident> = path.segments.iter().map(|s| &s.ident).collect();
    if segs.len() == 2 {
        let first = segs[0];
        let second = segs[1];
        if (first == "tokio" && second == "test")
            || (first == "async_std" && second == "test")
            || (first == "kani" && second == "proof")
        {
            return true;
        }
    }

    if path.is_ident("cfg") {
        let mut has_test = false;
        let _ = attr.parse_nested_meta(|meta| {
            if nested_meta_contains_test(&meta) {
                has_test = true;
            }
            Ok(())
        });
        if has_test {
            return true;
        }
    }

    false
}

fn nested_meta_contains_test(meta: &syn::meta::ParseNestedMeta) -> bool {
    if meta.path.is_ident("test") {
        return true;
    }
    if meta.path.is_ident("not") {
        return false;
    }
    let mut found = false;
    let _ = meta.parse_nested_meta(|inner| {
        if nested_meta_contains_test(&inner) {
            found = true;
        }
        Ok(())
    });
    found
}

/// Determines if an AST item represents a test function, proof, or test-only module/definition.
pub fn is_test_item(item: &syn::Item) -> bool {
    let attrs = match item {
        syn::Item::Const(it) => &it.attrs,
        syn::Item::Enum(it) => &it.attrs,
        syn::Item::ExternCrate(it) => &it.attrs,
        syn::Item::Fn(it) => &it.attrs,
        syn::Item::ForeignMod(it) => &it.attrs,
        syn::Item::Impl(it) => &it.attrs,
        syn::Item::Macro(it) => &it.attrs,
        syn::Item::Mod(it) => &it.attrs,
        syn::Item::Static(it) => &it.attrs,
        syn::Item::Struct(it) => &it.attrs,
        syn::Item::Trait(it) => &it.attrs,
        syn::Item::TraitAlias(it) => &it.attrs,
        syn::Item::Type(it) => &it.attrs,
        syn::Item::Union(it) => &it.attrs,
        syn::Item::Use(it) => &it.attrs,
        _ => return false,
    };
    attrs.iter().any(is_test_attribute)
}

fn filter_items(items: &mut Vec<syn::Item>) {
    items.retain(|item| !is_test_item(item));
    for item in items.iter_mut() {
        match item {
            syn::Item::Mod(item_mod) => {
                if let Some((_, ref mut sub_items)) = item_mod.content {
                    filter_items(sub_items);
                }
            }
            syn::Item::Impl(item_impl) => {
                item_impl.items.retain(|impl_item| {
                    if let syn::ImplItem::Fn(fn_item) = impl_item {
                        !fn_item.attrs.iter().any(is_test_attribute)
                    } else {
                        true
                    }
                });
            }
            _ => {}
        }
    }
}

/// AST visitor that clears implementation bodies while preserving interface contracts.
#[derive(Debug, Default)]
pub struct SkeletonStripper;

impl VisitMut for SkeletonStripper {
    fn visit_item_fn_mut(&mut self, item_fn: &mut syn::ItemFn) {
        item_fn.block.stmts.clear();
        syn::visit_mut::visit_item_fn_mut(self, item_fn);
    }

    fn visit_impl_item_fn_mut(&mut self, impl_fn: &mut syn::ImplItemFn) {
        impl_fn.block.stmts.clear();
        syn::visit_mut::visit_impl_item_fn_mut(self, impl_fn);
    }

    fn visit_trait_item_fn_mut(&mut self, trait_fn: &mut syn::TraitItemFn) {
        if let Some(default_block) = &mut trait_fn.default {
            default_block.stmts.clear();
        }
        syn::visit_mut::visit_trait_item_fn_mut(self, trait_fn);
    }
}

/// Strips implementation bodies in place from a parsed syn::File syntax tree.
/// Tests are omitted by default.
pub fn strip_file_ast(file: &mut syn::File) {
    strip_file_ast_with_options(file, true);
}

/// Strips implementation bodies in place from a parsed syn::File syntax tree with options.
pub fn strip_file_ast_with_options(file: &mut syn::File, omit_tests: bool) {
    if omit_tests {
        filter_items(&mut file.items);
    }
    let mut stripper = SkeletonStripper;
    stripper.visit_file_mut(file);
}

/// Parses Rust source code, strips all function and method bodies, and formats the skeleton.
/// Tests are omitted by default.
pub fn extract_skeleton(source_code: &str) -> Result<String, DistillError> {
    extract_skeleton_with_options(source_code, true)
}

/// Parses Rust source code, strips all function and method bodies, and formats the skeleton with options.
pub fn extract_skeleton_with_options(
    source_code: &str,
    omit_tests: bool,
) -> Result<String, DistillError> {
    let mut ast: syn::File =
        syn::parse_str(source_code).map_err(|source| DistillError::ParseFailure {
            path: Path::new("<memory>").to_path_buf(),
            source,
        })?;

    strip_file_ast_with_options(&mut ast, omit_tests);

    Ok(prettyplease::unparse(&ast))
}

/// Reads a source file from disk, parses it, and generates its type skeleton.
/// Tests are omitted by default.
pub fn extract_file_skeleton(path: &Path) -> Result<String, DistillError> {
    extract_file_skeleton_with_options(path, true)
}

/// Reads a source file from disk, parses it, and generates its type skeleton with options.
pub fn extract_file_skeleton_with_options(
    path: &Path,
    omit_tests: bool,
) -> Result<String, DistillError> {
    let content = std::fs::read_to_string(path).map_err(DistillError::Io)?;
    let mut ast: syn::File =
        syn::parse_str(&content).map_err(|source| DistillError::ParseFailure {
            path: path.to_path_buf(),
            source,
        })?;

    strip_file_ast_with_options(&mut ast, omit_tests);

    Ok(prettyplease::unparse(&ast))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skeleton_stripping() {
        let code = r#"
            pub struct Engine {
                pub id: u64,
                pub name: String,
            }

            impl Engine {
                pub fn new(id: u64) -> Self {
                    let name = format!("engine-{}", id);
                    Self { id, name }
                }

                pub fn start(&self) -> bool {
                    println!("Starting engine");
                    true
                }
            }
        "#;

        let skeleton = extract_skeleton(code).unwrap();
        assert!(skeleton.contains("pub struct Engine"));
        assert!(skeleton.contains("pub fn new(id: u64) -> Self"));
        assert!(skeleton.contains("pub fn start(&self) -> bool"));
        // Method bodies must be stripped
        assert!(!skeleton.contains("println!"));
        assert!(!skeleton.contains("format!"));
    }

    #[test]
    fn test_skeleton_stripping_omit_tests_true() {
        let code = r#"
            pub struct Engine {
                pub id: u64,
            }

            impl Engine {
                pub fn run(&self) -> bool {
                    true
                }

                #[test]
                fn test_in_impl() {
                    assert!(true);
                }
            }

            #[test]
            fn test_unit() {
                assert_eq!(1, 1);
            }

            #[tokio::test]
            async fn test_async() {
                assert!(true);
            }

            #[kani::proof]
            fn verify_proof() {
                assert!(true);
            }

            #[cfg(test)]
            mod tests {
                use super::*;

                #[test]
                fn internal_test() {
                    assert!(true);
                }
            }
        "#;

        let skeleton = extract_skeleton_with_options(code, true).unwrap();
        assert!(skeleton.contains("pub struct Engine"));
        assert!(skeleton.contains("pub fn run(&self) -> bool"));
        assert!(!skeleton.contains("test_in_impl"));
        assert!(!skeleton.contains("test_unit"));
        assert!(!skeleton.contains("test_async"));
        assert!(!skeleton.contains("verify_proof"));
        assert!(!skeleton.contains("mod tests"));
        assert!(!skeleton.contains("internal_test"));
        assert!(!skeleton.contains("assert"));
    }

    #[test]
    fn test_skeleton_stripping_omit_tests_false() {
        let code = r#"
            pub struct Engine {
                pub id: u64,
            }

            #[test]
            fn test_unit() {
                assert_eq!(1, 1);
            }

            #[cfg(test)]
            mod tests {
                #[test]
                fn internal_test() {
                    assert!(true);
                }
            }
        "#;

        let skeleton = extract_skeleton_with_options(code, false).unwrap();
        assert!(skeleton.contains("pub struct Engine"));
        assert!(skeleton.contains("fn test_unit()"));
        assert!(skeleton.contains("mod tests"));
        assert!(skeleton.contains("fn internal_test()"));
        // Method bodies must be stripped even when tests are retained
        assert!(!skeleton.contains("assert_eq!"));
        assert!(!skeleton.contains("assert!"));
    }

    #[test]
    fn test_is_test_attribute_detection() {
        let test_attr: syn::Attribute = syn::parse_quote!(#[test]);
        assert!(is_test_attribute(&test_attr));

        let tokio_attr: syn::Attribute = syn::parse_quote!(#[tokio::test]);
        assert!(is_test_attribute(&tokio_attr));

        let kani_attr: syn::Attribute = syn::parse_quote!(#[kani::proof]);
        assert!(is_test_attribute(&kani_attr));

        let cfg_test_attr: syn::Attribute = syn::parse_quote!(#[cfg(test)]);
        assert!(is_test_attribute(&cfg_test_attr));

        let cfg_all_test: syn::Attribute = syn::parse_quote!(#[cfg(all(test, feature = "f"))]);
        assert!(is_test_attribute(&cfg_all_test));

        let cfg_not_test: syn::Attribute = syn::parse_quote!(#[cfg(not(test))]);
        assert!(!is_test_attribute(&cfg_not_test));

        let derive_attr: syn::Attribute = syn::parse_quote!(#[derive(Debug)]);
        assert!(!is_test_attribute(&derive_attr));
    }
}
