//! Phase-1 Rust AST fact extractor and recursive macro call scanner.

use crate::error::DistillError;
use crate::indexer::{ContractInfo, RouteBinding, SymbolDef};
use proc_macro2::{Delimiter, TokenStream, TokenTree};
use quote::ToTokens;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::Visit;

/// Kind of 1-hop standard container type recognized during local type tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContainerKind {
    Arc,
    Box,
    Rc,
    Option,
    Result,
    Vec,
    Mutex,
    RwLock,
    RefCell,
}

impl ContainerKind {
    /// Attempts to parse an identifier into a known container kind.
    pub fn from_ident(s: &str) -> Option<Self> {
        match s {
            "Arc" => Some(ContainerKind::Arc),
            "Box" => Some(ContainerKind::Box),
            "Rc" => Some(ContainerKind::Rc),
            "Option" => Some(ContainerKind::Option),
            "Result" => Some(ContainerKind::Result),
            "Vec" => Some(ContainerKind::Vec),
            "Mutex" => Some(ContainerKind::Mutex),
            "RwLock" => Some(ContainerKind::RwLock),
            "RefCell" => Some(ContainerKind::RefCell),
            _ => None,
        }
    }
}

/// Syntactic representation of a resolved or partially resolved type reference.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TypeRef {
    Known {
        crate_or_pkg: String,
        type_name: String,
    },
    Applied {
        container: ContainerKind,
        inner: Box<TypeRef>,
    },
    Unknown,
}

impl TypeRef {
    /// Unwraps 1-hop container layer if present, returning the underlying type reference.
    pub fn unwrap_container(&self) -> &TypeRef {
        match self {
            TypeRef::Applied { inner, .. } => inner.as_ref(),
            other => other,
        }
    }
}

/// Target representation for unresolved Phase-1 call sites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RawCalleeTarget {
    Path(String),
    Method {
        receiver: TypeRef,
        method_name: String,
    },
    FieldChain {
        receiver: TypeRef,
        fields: Vec<String>,
        method_name: String,
    },
}

/// Unresolved call invocation extracted during Phase-1 AST analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawCallSite {
    pub caller_fqn: String,
    pub call_line: u32,
    pub target: RawCalleeTarget,
}

/// Re-export entry extracted from module manifests and barrel declarations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExportEntry {
    Local(String),
    NamedReExport {
        specifier: String,
        imported: String,
        exported: String,
    },
    StarReExport {
        specifier: String,
    },
}

/// Phase-1 syntactic extraction facts for a single source file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileFacts {
    pub file_path: PathBuf,
    pub rel_path: PathBuf,
    pub crate_or_pkg: String,
    pub symbols: Vec<SymbolDef>,
    pub contracts: Vec<ContractInfo>,
    pub routes: Vec<RouteBinding>,
    pub field_types: HashMap<String, HashMap<String, TypeRef>>,
    pub return_types: HashMap<String, TypeRef>,
    pub exports: Vec<ExportEntry>,
    pub calls: Vec<RawCallSite>,
}

/// Configuration options for Phase-1 Rust AST fact extraction.
#[derive(Debug, Clone)]
pub struct RustFactOptions<'a> {
    pub file_path: &'a Path,
    pub rel_path: &'a Path,
    pub crate_name: &'a str,
    pub omit_tests: bool,
}

/// Derives the base module path components from crate name and relative path.
pub fn derive_base_module_path(crate_name: &str, rel_path: &Path) -> Vec<String> {
    let normalized_crate = crate_name.replace('-', "_");
    let mut parts = vec![normalized_crate];

    let components: Vec<_> = rel_path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();

    let src_idx = components.iter().position(|&c| c == "src");
    let sub_components = match src_idx {
        Some(idx) => &components[idx + 1..],
        None => &components[..],
    };

    if let Some((last, init)) = sub_components.split_last() {
        for &comp in init {
            parts.push(comp.replace('-', "_"));
        }
        let stem = Path::new(last)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(last);
        if stem != "lib" && stem != "main" && stem != "mod" {
            parts.push(stem.replace('-', "_"));
        }
    }

    parts
}

/// Parses a syn::Type into a TypeRef representation.
fn parse_syn_type(
    ty: &syn::Type,
    crate_name: &str,
    current_mod: &str,
    current_self: Option<&str>,
) -> TypeRef {
    match ty {
        syn::Type::Reference(r) => parse_syn_type(&r.elem, crate_name, current_mod, current_self),
        syn::Type::Paren(p) => parse_syn_type(&p.elem, crate_name, current_mod, current_self),
        syn::Type::Path(p) => parse_type_path(&p.path, crate_name, current_mod, current_self),
        _ => TypeRef::Unknown,
    }
}

/// Parses a syn::Path into a TypeRef representation.
fn parse_type_path(
    path: &syn::Path,
    crate_name: &str,
    current_mod: &str,
    current_self: Option<&str>,
) -> TypeRef {
    let seg = match path.segments.last() {
        Some(s) => s,
        None => return TypeRef::Unknown,
    };
    let ident_str = seg.ident.to_string();

    if let Some(container) = ContainerKind::from_ident(&ident_str) {
        if let syn::PathArguments::AngleBracketed(ref args) = seg.arguments {
            for arg in &args.args {
                if let syn::GenericArgument::Type(inner_ty) = arg {
                    let inner = parse_syn_type(inner_ty, crate_name, current_mod, current_self);
                    return TypeRef::Applied {
                        container,
                        inner: Box::new(inner),
                    };
                }
            }
        }
    }

    if ident_str == "Self" {
        if let Some(self_ty) = current_self {
            return TypeRef::Known {
                crate_or_pkg: crate_name.to_string(),
                type_name: self_ty.to_string(),
            };
        }
    }

    if path.segments.len() == 1 {
        if ident_str.chars().next().is_some_and(|c| c.is_uppercase()) {
            return TypeRef::Known {
                crate_or_pkg: current_mod.to_string(),
                type_name: ident_str,
            };
        }
    } else {
        let prefix: Vec<_> = path
            .segments
            .iter()
            .take(path.segments.len().saturating_sub(1))
            .map(|s| {
                if s.ident == "crate" {
                    crate_name.to_string()
                } else {
                    s.ident.to_string()
                }
            })
            .collect();
        return TypeRef::Known {
            crate_or_pkg: prefix.join("::"),
            type_name: ident_str,
        };
    }

    TypeRef::Unknown
}

/// Helper context for recursive macro token scanning.
struct MacroScanContext<'a> {
    caller_fqn: &'a str,
    current_self_type: Option<&'a str>,
    crate_name: &'a str,
    local_vars: &'a HashMap<String, TypeRef>,
}

/// Recursively scans token trees inside a macro invocation for method and path calls.
fn scan_macro_tokens(
    ctx: &MacroScanContext<'_>,
    tokens: TokenStream,
    calls: &mut Vec<RawCallSite>,
) {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    let len = trees.len();
    let mut i = 0;

    while i < len {
        match &trees[i] {
            TokenTree::Group(g) => {
                scan_macro_tokens(ctx, g.stream(), calls);
                i += 1;
            }
            TokenTree::Ident(id) => {
                let rec_name = id.to_string();
                if i + 3 < len
                    && is_punct(&trees[i + 1], '.')
                    && is_ident(&trees[i + 2])
                    && is_paren_group(&trees[i + 3])
                {
                    let method_name = trees[i + 2].to_string();
                    let line = trees[i + 2].span().start().line as u32;
                    record_macro_method_call(ctx, &rec_name, &method_name, line, calls);
                    if let TokenTree::Group(g) = &trees[i + 3] {
                        scan_macro_tokens(ctx, g.stream(), calls);
                    }
                    i += 4;
                } else if i + 1 < len && is_paren_group(&trees[i + 1]) {
                    let fn_name = id.to_string();
                    let line = id.span().start().line as u32;
                    if !is_rust_keyword(&fn_name) {
                        calls.push(RawCallSite {
                            caller_fqn: ctx.caller_fqn.to_string(),
                            call_line: line,
                            target: RawCalleeTarget::Path(fn_name),
                        });
                    }
                    if let TokenTree::Group(g) = &trees[i + 1] {
                        scan_macro_tokens(ctx, g.stream(), calls);
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => {
                i += 1;
            }
        }
    }
}

fn is_punct(tt: &TokenTree, ch: char) -> bool {
    matches!(tt, TokenTree::Punct(p) if p.as_char() == ch)
}

fn is_ident(tt: &TokenTree) -> bool {
    matches!(tt, TokenTree::Ident(_))
}

fn is_paren_group(tt: &TokenTree) -> bool {
    matches!(tt, TokenTree::Group(g) if g.delimiter() == Delimiter::Parenthesis)
}

fn is_rust_keyword(s: &str) -> bool {
    matches!(
        s,
        "if" | "while"
            | "match"
            | "for"
            | "loop"
            | "return"
            | "let"
            | "Some"
            | "Ok"
            | "Err"
            | "vec"
            | "format"
            | "println"
            | "eprintln"
            | "assert"
            | "assert_eq"
            | "panic"
    )
}

fn record_macro_method_call(
    ctx: &MacroScanContext<'_>,
    rec_name: &str,
    method_name: &str,
    line: u32,
    calls: &mut Vec<RawCallSite>,
) {
    if rec_name == "self" {
        if let Some(self_ty) = ctx.current_self_type {
            calls.push(RawCallSite {
                caller_fqn: ctx.caller_fqn.to_string(),
                call_line: line,
                target: RawCalleeTarget::Method {
                    receiver: TypeRef::Known {
                        crate_or_pkg: ctx.crate_name.to_string(),
                        type_name: self_ty.to_string(),
                    },
                    method_name: method_name.to_string(),
                },
            });
        }
    } else if let Some(receiver) = ctx.local_vars.get(rec_name) {
        calls.push(RawCallSite {
            caller_fqn: ctx.caller_fqn.to_string(),
            call_line: line,
            target: RawCalleeTarget::Method {
                receiver: receiver.unwrap_container().clone(),
                method_name: method_name.to_string(),
            },
        });
    }
}

/// Decomposes an expression into its root base expression and a chain of field names.
fn decompose_receiver(expr: &syn::Expr) -> (Option<&syn::Expr>, Vec<String>) {
    let mut fields = Vec::new();
    let mut curr = expr;

    while let syn::Expr::Field(f) = curr {
        fields.push(f.member.to_token_stream().to_string());
        curr = &f.base;
    }

    fields.reverse();
    (Some(curr), fields)
}

/// Infers the TypeRef for an initialization expression (literals and constructors).
fn infer_expr_type(
    expr: &syn::Expr,
    crate_name: &str,
    current_mod: &str,
    current_self: Option<&str>,
    local_vars: &HashMap<String, TypeRef>,
) -> TypeRef {
    match expr {
        syn::Expr::Path(p) => {
            if let Some(ident) = p.path.get_ident() {
                if let Some(ty) = local_vars.get(&ident.to_string()) {
                    return ty.clone();
                }
            }
            parse_type_path(&p.path, crate_name, current_mod, current_self)
        }
        syn::Expr::Struct(s) => parse_type_path(&s.path, crate_name, current_mod, current_self),
        syn::Expr::Reference(r) => {
            infer_expr_type(&r.expr, crate_name, current_mod, current_self, local_vars)
        }
        syn::Expr::Call(call) => {
            if let syn::Expr::Path(ref p) = *call.func {
                if let Some(seg) = p.path.segments.last() {
                    let method = seg.ident.to_string();
                    if p.path.segments.len() >= 2 {
                        let prev_seg = &p.path.segments[p.path.segments.len() - 2];
                        let prev_ident = prev_seg.ident.to_string();
                        if let Some(container) = ContainerKind::from_ident(&prev_ident) {
                            if let Some(first_arg) = call.args.first() {
                                let inner = infer_expr_type(
                                    first_arg,
                                    crate_name,
                                    current_mod,
                                    current_self,
                                    local_vars,
                                );
                                return TypeRef::Applied {
                                    container,
                                    inner: Box::new(inner),
                                };
                            }
                        } else if matches!(
                            method.as_str(),
                            "new" | "default" | "create" | "with_capacity"
                        ) {
                            return TypeRef::Known {
                                crate_or_pkg: current_mod.to_string(),
                                type_name: prev_ident,
                            };
                        }
                    }
                }
            }
            TypeRef::Unknown
        }
        _ => TypeRef::Unknown,
    }
}

/// AST visitor that extracts symbols, routes, contracts, and call facts from a Rust source file.
struct RustFactVisitor {
    file_path_str: String,
    crate_name: String,
    omit_tests: bool,
    module_stack: Vec<String>,
    current_self_type: Option<String>,
    current_caller_fqn: Option<String>,
    local_vars: HashMap<String, TypeRef>,
    symbols: Vec<SymbolDef>,
    contracts: Vec<ContractInfo>,
    routes: Vec<RouteBinding>,
    field_types: HashMap<String, HashMap<String, TypeRef>>,
    return_types: HashMap<String, TypeRef>,
    calls: Vec<RawCallSite>,
}

impl RustFactVisitor {
    fn new(opts: &RustFactOptions<'_>) -> Self {
        let base_module = derive_base_module_path(opts.crate_name, opts.rel_path);
        Self {
            file_path_str: opts.file_path.to_string_lossy().to_string(),
            crate_name: opts.crate_name.replace('-', "_"),
            omit_tests: opts.omit_tests,
            module_stack: base_module,
            current_self_type: None,
            current_caller_fqn: None,
            local_vars: HashMap::new(),
            symbols: Vec::new(),
            contracts: Vec::new(),
            routes: Vec::new(),
            field_types: HashMap::new(),
            return_types: HashMap::new(),
            calls: Vec::new(),
        }
    }

    fn current_module(&self) -> String {
        self.module_stack.join("::")
    }

    fn record_route_if_matching(&mut self, attrs: &[syn::Attribute], name: &str, line: usize) {
        for attr in attrs {
            let attr_str = attr.to_token_stream().to_string();
            if attr_str.contains("tauri :: command") || attr_str.contains("tauri::command") {
                self.routes.push(RouteBinding::new(
                    name.to_string(),
                    name.to_string(),
                    self.file_path_str.clone(),
                    line,
                    true,
                ));
            } else if attr_str.contains("get")
                || attr_str.contains("post")
                || attr_str.contains("put")
                || attr_str.contains("delete")
            {
                let route_path =
                    extract_attr_literal(&attr_str).unwrap_or_else(|| name.to_string());
                self.routes.push(RouteBinding::new(
                    route_path,
                    name.to_string(),
                    self.file_path_str.clone(),
                    line,
                    true,
                ));
            }
        }
    }
}

fn extract_attr_literal(attr_str: &str) -> Option<String> {
    let start = attr_str.find('"')? + 1;
    let end = attr_str[start..].find('"')? + start;
    Some(attr_str[start..end].to_string())
}

impl Visit<'_> for RustFactVisitor {
    fn visit_item_mod(&mut self, node: &syn::ItemMod) {
        if self.omit_tests && node.attrs.iter().any(crate::skeleton::is_test_attribute) {
            return;
        }
        if node.content.is_some() {
            let mod_name = node.ident.to_string();
            self.module_stack.push(mod_name.clone());
            let line = node.span().start().line;
            let end_line = node.span().end().line;
            let qualified_name = self.current_module();
            self.symbols.push(SymbolDef {
                name: mod_name,
                kind: "mod".to_string(),
                file: self.file_path_str.clone(),
                line,
                language: "rust".to_string(),
                end_line,
                qualified_name,
            });
            syn::visit::visit_item_mod(self, node);
            self.module_stack.pop();
        } else {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_struct(&mut self, node: &syn::ItemStruct) {
        if self.omit_tests && node.attrs.iter().any(crate::skeleton::is_test_attribute) {
            return;
        }
        let name = node.ident.to_string();
        let line = node.span().start().line;
        let end_line = node.span().end().line;
        let qualified_name = format!("{}::{}", self.current_module(), name);

        self.symbols.push(SymbolDef {
            name: name.clone(),
            kind: "struct".to_string(),
            file: self.file_path_str.clone(),
            line,
            language: "rust".to_string(),
            end_line,
            qualified_name,
        });

        if let syn::Fields::Named(ref fields) = node.fields {
            let mut map = HashMap::new();
            for field in &fields.named {
                if let Some(ref field_ident) = field.ident {
                    let ty_ref = parse_syn_type(
                        &field.ty,
                        &self.crate_name,
                        &self.current_module(),
                        Some(&name),
                    );
                    map.insert(field_ident.to_string(), ty_ref);
                }
            }
            self.field_types.insert(name, map);
        }

        syn::visit::visit_item_struct(self, node);
    }

    fn visit_item_enum(&mut self, node: &syn::ItemEnum) {
        if self.omit_tests && node.attrs.iter().any(crate::skeleton::is_test_attribute) {
            return;
        }
        let name = node.ident.to_string();
        let line = node.span().start().line;
        let end_line = node.span().end().line;
        let qualified_name = format!("{}::{}", self.current_module(), name);

        self.symbols.push(SymbolDef {
            name,
            kind: "enum".to_string(),
            file: self.file_path_str.clone(),
            line,
            language: "rust".to_string(),
            end_line,
            qualified_name,
        });

        syn::visit::visit_item_enum(self, node);
    }

    fn visit_item_trait(&mut self, node: &syn::ItemTrait) {
        if self.omit_tests && node.attrs.iter().any(crate::skeleton::is_test_attribute) {
            return;
        }
        let name = node.ident.to_string();
        let line = node.span().start().line;
        let end_line = node.span().end().line;
        let qualified_name = format!("{}::{}", self.current_module(), name);

        self.symbols.push(SymbolDef {
            name,
            kind: "trait".to_string(),
            file: self.file_path_str.clone(),
            line,
            language: "rust".to_string(),
            end_line,
            qualified_name,
        });

        syn::visit::visit_item_trait(self, node);
    }

    fn visit_item_type(&mut self, node: &syn::ItemType) {
        if self.omit_tests && node.attrs.iter().any(crate::skeleton::is_test_attribute) {
            return;
        }
        let name = node.ident.to_string();
        let line = node.span().start().line;
        let end_line = node.span().end().line;
        let qualified_name = format!("{}::{}", self.current_module(), name);

        self.symbols.push(SymbolDef {
            name,
            kind: "type".to_string(),
            file: self.file_path_str.clone(),
            line,
            language: "rust".to_string(),
            end_line,
            qualified_name,
        });

        syn::visit::visit_item_type(self, node);
    }

    fn visit_item_fn(&mut self, node: &syn::ItemFn) {
        if self.omit_tests && node.attrs.iter().any(crate::skeleton::is_test_attribute) {
            return;
        }
        let name = node.sig.ident.to_string();
        let line = node.span().start().line;
        let end_line = node.span().end().line;
        let qualified_name = format!("{}::{}", self.current_module(), name);

        self.symbols.push(SymbolDef {
            name: name.clone(),
            kind: "fn".to_string(),
            file: self.file_path_str.clone(),
            line,
            language: "rust".to_string(),
            end_line,
            qualified_name: qualified_name.clone(),
        });

        self.record_route_if_matching(&node.attrs, &qualified_name, line);

        if let syn::ReturnType::Type(_, ref ty) = node.sig.output {
            let ret_ty = parse_syn_type(ty, &self.crate_name, &self.current_module(), None);
            self.return_types.insert(qualified_name.clone(), ret_ty);
        }

        let mut vars = HashMap::new();
        for input in &node.sig.inputs {
            if let syn::FnArg::Typed(pt) = input {
                if let syn::Pat::Ident(ref id) = *pt.pat {
                    let ty_ref =
                        parse_syn_type(&pt.ty, &self.crate_name, &self.current_module(), None);
                    vars.insert(id.ident.to_string(), ty_ref);
                }
            }
        }

        let prev_caller = self.current_caller_fqn.replace(qualified_name);
        let prev_vars = std::mem::replace(&mut self.local_vars, vars);
        syn::visit::visit_item_fn(self, node);
        self.current_caller_fqn = prev_caller;
        self.local_vars = prev_vars;
    }

    fn visit_item_impl(&mut self, node: &syn::ItemImpl) {
        if self.omit_tests && node.attrs.iter().any(crate::skeleton::is_test_attribute) {
            return;
        }
        let target_type = node.self_ty.to_token_stream().to_string().replace(' ', "");
        let self_ident = if let syn::Type::Path(ref p) = *node.self_ty {
            p.path.segments.last().map(|s| s.ident.to_string())
        } else {
            None
        };

        if let Some((_, ref trait_path, _)) = node.trait_ {
            let trait_name = trait_path.to_token_stream().to_string().replace(' ', "");
            let mut methods = Vec::new();
            for item in &node.items {
                if let syn::ImplItem::Fn(m) = item {
                    if !self.omit_tests || !m.attrs.iter().any(crate::skeleton::is_test_attribute) {
                        methods.push(m.sig.to_token_stream().to_string());
                    }
                }
            }
            self.contracts.push(ContractInfo {
                trait_name,
                target_type,
                methods,
                file: self.file_path_str.clone(),
            });
        }

        let prev_self = self.current_self_type.take();
        self.current_self_type = self_ident.or(prev_self.clone());
        syn::visit::visit_item_impl(self, node);
        self.current_self_type = prev_self;
    }

    fn visit_impl_item_fn(&mut self, node: &syn::ImplItemFn) {
        if self.omit_tests && node.attrs.iter().any(crate::skeleton::is_test_attribute) {
            return;
        }
        let name = node.sig.ident.to_string();
        let line = node.span().start().line;
        let end_line = node.span().end().line;
        let qualified_name = if let Some(ref self_ty) = self.current_self_type {
            format!("{}::{}::{}", self.current_module(), self_ty, name)
        } else {
            format!("{}::{}", self.current_module(), name)
        };

        self.symbols.push(SymbolDef {
            name: name.clone(),
            kind: "fn".to_string(),
            file: self.file_path_str.clone(),
            line,
            language: "rust".to_string(),
            end_line,
            qualified_name: qualified_name.clone(),
        });

        self.record_route_if_matching(&node.attrs, &qualified_name, line);

        if let syn::ReturnType::Type(_, ref ty) = node.sig.output {
            let ret_ty = parse_syn_type(
                ty,
                &self.crate_name,
                &self.current_module(),
                self.current_self_type.as_deref(),
            );
            self.return_types.insert(qualified_name.clone(), ret_ty);
        }

        let mut vars = HashMap::new();
        for input in &node.sig.inputs {
            match input {
                syn::FnArg::Receiver(_) => {
                    if let Some(ref self_ty) = self.current_self_type {
                        vars.insert(
                            "self".to_string(),
                            TypeRef::Known {
                                crate_or_pkg: self.crate_name.clone(),
                                type_name: self_ty.clone(),
                            },
                        );
                    }
                }
                syn::FnArg::Typed(pt) => {
                    if let syn::Pat::Ident(ref id) = *pt.pat {
                        let ty_ref = parse_syn_type(
                            &pt.ty,
                            &self.crate_name,
                            &self.current_module(),
                            self.current_self_type.as_deref(),
                        );
                        vars.insert(id.ident.to_string(), ty_ref);
                    }
                }
            }
        }

        let prev_caller = self.current_caller_fqn.replace(qualified_name);
        let prev_vars = std::mem::replace(&mut self.local_vars, vars);
        syn::visit::visit_impl_item_fn(self, node);
        self.current_caller_fqn = prev_caller;
        self.local_vars = prev_vars;
    }

    fn visit_local(&mut self, node: &syn::Local) {
        if let syn::Pat::Type(ref pt) = node.pat {
            if let syn::Pat::Ident(ref id) = *pt.pat {
                let ty_ref = parse_syn_type(
                    &pt.ty,
                    &self.crate_name,
                    &self.current_module(),
                    self.current_self_type.as_deref(),
                );
                self.local_vars.insert(id.ident.to_string(), ty_ref);
            }
        } else if let syn::Pat::Ident(ref id) = node.pat {
            if let Some(ref init) = node.init {
                let inferred = infer_expr_type(
                    &init.expr,
                    &self.crate_name,
                    &self.current_module(),
                    self.current_self_type.as_deref(),
                    &self.local_vars,
                );
                if inferred != TypeRef::Unknown {
                    self.local_vars.insert(id.ident.to_string(), inferred);
                }
            }
        }
        syn::visit::visit_local(self, node);
    }

    fn visit_expr_call(&mut self, node: &syn::ExprCall) {
        if let Some(ref caller) = self.current_caller_fqn {
            let line = node.span().start().line as u32;
            if let syn::Expr::Path(ref ep) = *node.func {
                let path_str = ep.path.to_token_stream().to_string().replace(' ', "");
                if ep.path.segments.len() >= 2 {
                    let first_seg = ep.path.segments.first().unwrap().ident.to_string();
                    let method = ep.path.segments.last().unwrap().ident.to_string();
                    if first_seg == "Self" {
                        if let Some(ref self_ty) = self.current_self_type {
                            self.calls.push(RawCallSite {
                                caller_fqn: caller.clone(),
                                call_line: line,
                                target: RawCalleeTarget::Method {
                                    receiver: TypeRef::Known {
                                        crate_or_pkg: self.crate_name.clone(),
                                        type_name: self_ty.clone(),
                                    },
                                    method_name: method,
                                },
                            });
                        }
                    } else if first_seg.chars().next().is_some_and(|c| c.is_uppercase()) {
                        self.calls.push(RawCallSite {
                            caller_fqn: caller.clone(),
                            call_line: line,
                            target: RawCalleeTarget::Method {
                                receiver: TypeRef::Known {
                                    crate_or_pkg: self.current_module(),
                                    type_name: first_seg,
                                },
                                method_name: method,
                            },
                        });
                    } else {
                        self.calls.push(RawCallSite {
                            caller_fqn: caller.clone(),
                            call_line: line,
                            target: RawCalleeTarget::Path(path_str),
                        });
                    }
                } else {
                    self.calls.push(RawCallSite {
                        caller_fqn: caller.clone(),
                        call_line: line,
                        target: RawCalleeTarget::Path(path_str),
                    });
                }
            }
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &syn::ExprMethodCall) {
        if let Some(ref caller) = self.current_caller_fqn {
            let line = node.span().start().line as u32;
            let method_name = node.method.to_string();
            let (base_expr, field_chain) = decompose_receiver(&node.receiver);

            if let Some(syn::Expr::Path(p)) = base_expr {
                if let Some(id) = p.path.get_ident() {
                    let id_str = id.to_string();
                    let receiver = if id_str == "self" {
                        self.current_self_type.as_ref().map(|st| TypeRef::Known {
                            crate_or_pkg: self.crate_name.clone(),
                            type_name: st.clone(),
                        })
                    } else {
                        self.local_vars.get(&id_str).cloned()
                    };

                    if let Some(rec) = receiver {
                        let unwrapped = rec.unwrap_container().clone();
                        if field_chain.is_empty() {
                            self.calls.push(RawCallSite {
                                caller_fqn: caller.clone(),
                                call_line: line,
                                target: RawCalleeTarget::Method {
                                    receiver: unwrapped,
                                    method_name,
                                },
                            });
                        } else if field_chain.len() <= 4 {
                            self.calls.push(RawCallSite {
                                caller_fqn: caller.clone(),
                                call_line: line,
                                target: RawCalleeTarget::FieldChain {
                                    receiver: unwrapped,
                                    fields: field_chain,
                                    method_name,
                                },
                            });
                        }
                    }
                }
            }
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_macro(&mut self, node: &syn::Macro) {
        if let Some(ref caller) = self.current_caller_fqn {
            let ctx = MacroScanContext {
                caller_fqn: caller,
                current_self_type: self.current_self_type.as_deref(),
                crate_name: &self.crate_name,
                local_vars: &self.local_vars,
            };
            scan_macro_tokens(&ctx, node.tokens.clone(), &mut self.calls);
        }
        syn::visit::visit_macro(self, node);
    }
}

/// Extracts syntactic facts and call sites from a Rust source file.
pub fn extract_rust_file_facts(
    opts: &RustFactOptions<'_>,
    content: &str,
) -> Result<FileFacts, DistillError> {
    let ast =
        syn::parse_str::<syn::File>(content).map_err(|source| DistillError::ParseFailure {
            path: opts.file_path.to_path_buf(),
            source,
        })?;

    let mut visitor = RustFactVisitor::new(opts);
    visitor.visit_file(&ast);

    Ok(FileFacts {
        file_path: opts.file_path.to_path_buf(),
        rel_path: opts.rel_path.to_path_buf(),
        crate_or_pkg: opts.crate_name.replace('-', "_"),
        symbols: visitor.symbols,
        contracts: visitor.contracts,
        routes: visitor.routes,
        field_types: visitor.field_types,
        return_types: visitor.return_types,
        exports: Vec::new(),
        calls: visitor.calls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_base_module_path() {
        assert_eq!(
            derive_base_module_path("cortx-distill", Path::new("src/indexer/rust_graph.rs")),
            vec!["cortx_distill", "indexer", "rust_graph"]
        );
        assert_eq!(
            derive_base_module_path("cortx-core", Path::new("crates/cortx-core/src/span.rs")),
            vec!["cortx_core", "span"]
        );
        assert_eq!(
            derive_base_module_path("cortx-distill", Path::new("src/indexer/mod.rs")),
            vec!["cortx_distill", "indexer"]
        );
        assert_eq!(
            derive_base_module_path("cortx-daemon", Path::new("src/lib.rs")),
            vec!["cortx_daemon"]
        );
    }

    #[test]
    fn test_extract_rust_file_facts_symbols_and_spans() {
        let code = r#"
pub struct Service {
    pub name: String,
}

impl Service {
    pub fn new(name: String) -> Self {
        Self { name }
    }

    pub fn run(&self) {
        self.helper();
    }

    fn helper(&self) {}
}

mod internal {
    pub fn sub_fn() {}
}
"#;
        let opts = RustFactOptions {
            file_path: Path::new("/repo/src/service.rs"),
            rel_path: Path::new("src/service.rs"),
            crate_name: "acme",
            omit_tests: false,
        };

        let facts = extract_rust_file_facts(&opts, code).expect("facts extraction");
        let sym_names: Vec<_> = facts
            .symbols
            .iter()
            .map(|s| s.qualified_name.as_str())
            .collect();
        assert!(sym_names.contains(&"acme::service::Service"));
        assert!(sym_names.contains(&"acme::service::Service::new"));
        assert!(sym_names.contains(&"acme::service::Service::run"));
        assert!(sym_names.contains(&"acme::service::internal"));
        assert!(sym_names.contains(&"acme::service::internal::sub_fn"));

        let run_sym = facts.symbols.iter().find(|s| s.name == "run").unwrap();
        assert!(run_sym.line > 0);
        assert!(run_sym.end_line >= run_sym.line);

        let self_call = facts.calls.iter().find(|c| {
            matches!(&c.target, RawCalleeTarget::Method { method_name, .. } if method_name == "helper")
        });
        assert!(self_call.is_some());
    }

    #[test]
    fn test_macro_call_scanner() {
        let code = r#"
pub fn run_loop(client: Client) {
    tokio::select! {
        _ = client.recv() => {
            handle_event();
        }
    }
}
"#;
        let opts = RustFactOptions {
            file_path: Path::new("/repo/src/loop.rs"),
            rel_path: Path::new("src/loop.rs"),
            crate_name: "acme",
            omit_tests: false,
        };

        let facts = extract_rust_file_facts(&opts, code).expect("facts extraction");
        let client_call = facts.calls.iter().find(|c| {
            matches!(&c.target, RawCalleeTarget::Method { method_name, .. } if method_name == "recv")
        });
        assert!(
            client_call.is_some(),
            "should extract client.recv() inside macro"
        );

        let handle_call = facts
            .calls
            .iter()
            .find(|c| matches!(&c.target, RawCalleeTarget::Path(p) if p == "handle_event"));
        assert!(
            handle_call.is_some(),
            "should extract handle_event() inside macro"
        );
    }
}
