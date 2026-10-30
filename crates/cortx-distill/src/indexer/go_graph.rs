//! Phase-1 Go AST fact, receiver chain, and route extractor using tree-sitter-go.

use crate::error::DistillError;
use crate::go_skeleton::{has_go_test_signature, is_go_test_function};
use crate::indexer::rust_graph::{ContainerKind, FileFacts, RawCallSite, RawCalleeTarget, TypeRef};
use crate::indexer::{ContractInfo, RouteBinding, SymbolDef};
use regex::Regex;
use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

static BACKEND_ROUTE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:r|router|e|echo|app|http|mux|api)\s*\.\s*(GET|POST|PUT|DELETE|PATCH|Get|Post|Put|Delete|Patch|HandleFunc)\s*\(\s*["'`]([^"'`]+)["'`]\s*,\s*([a-zA-Z0-9_\.]+)"#,
    )
    .expect("valid Go backend route regex")
});

/// Configuration options for Phase-1 Go AST fact extraction.
#[derive(Debug, Clone)]
pub struct GoFactOptions<'a> {
    pub file_path: &'a Path,
    pub rel_path: &'a Path,
    pub package_path: Option<&'a str>,
    pub omit_tests: bool,
}

/// Derives the package import path, falling back to workspace-relative directory path.
pub fn derive_go_package_path(opts: &GoFactOptions<'_>, ast_pkg: Option<&str>) -> String {
    if let Some(pkg) = opts.package_path {
        if !pkg.is_empty() {
            return pkg.to_string();
        }
    }
    let parent = opts.rel_path.parent().unwrap_or(Path::new(""));
    let s = parent.to_string_lossy().replace('\\', "/");
    let trimmed = s.strip_prefix("./").unwrap_or(&s);
    if trimmed.is_empty() {
        ast_pkg.unwrap_or("main").to_string()
    } else {
        trimmed.to_string()
    }
}

/// Method signature and receiver metadata for Go contract resolution.
#[derive(Debug, Clone)]
pub struct GoMethodInfo {
    pub receiver_type: String,
    pub is_pointer: bool,
    pub method_name: String,
    pub signature: String,
}

/// Interface contract specification extracted from Go source code.
#[derive(Debug, Clone)]
pub struct RawInterface {
    pub name: String,
    pub methods: Vec<(String, String)>,
}

struct LineIndex {
    line_starts: Vec<usize>,
}

impl LineIndex {
    fn new(content: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in content.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self { line_starts }
    }

    fn line_number(&self, offset: usize) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(idx) => idx + 1,
            Err(idx) => idx,
        }
    }
}

/// Computes implicit Go interface satisfaction contracts.
pub fn resolve_go_contracts(
    file_path: &str,
    structs: &[(String, usize)],
    interfaces: &[RawInterface],
    methods: &[GoMethodInfo],
) -> Vec<ContractInfo> {
    let mut contracts = Vec::new();

    for (s_name, _line) in structs {
        let value_methods: Vec<&GoMethodInfo> = methods
            .iter()
            .filter(|m| m.receiver_type == *s_name && !m.is_pointer)
            .collect();

        let pointer_methods: Vec<&GoMethodInfo> = methods
            .iter()
            .filter(|m| m.receiver_type == *s_name)
            .collect();

        for iface in interfaces {
            let req_methods = &iface.methods;
            if req_methods.is_empty() {
                continue;
            }

            let value_satisfies = req_methods
                .iter()
                .all(|(req_name, _)| value_methods.iter().any(|m| m.method_name == *req_name));

            if value_satisfies {
                let matched_sigs: Vec<String> = req_methods
                    .iter()
                    .filter_map(|(req_name, _)| {
                        value_methods
                            .iter()
                            .find(|m| m.method_name == *req_name)
                            .map(|m| m.signature.clone())
                    })
                    .collect();

                contracts.push(ContractInfo {
                    trait_name: iface.name.clone(),
                    target_type: s_name.clone(),
                    methods: matched_sigs,
                    file: file_path.to_string(),
                });
            }

            let pointer_satisfies = req_methods
                .iter()
                .all(|(req_name, _)| pointer_methods.iter().any(|m| m.method_name == *req_name));

            if pointer_satisfies && !value_satisfies {
                let matched_sigs: Vec<String> = req_methods
                    .iter()
                    .filter_map(|(req_name, _)| {
                        pointer_methods
                            .iter()
                            .find(|m| m.method_name == *req_name)
                            .map(|m| m.signature.clone())
                    })
                    .collect();

                contracts.push(ContractInfo {
                    trait_name: iface.name.clone(),
                    target_type: format!("*{}", s_name),
                    methods: matched_sigs,
                    file: file_path.to_string(),
                });
            }
        }
    }

    contracts
}

/// Extracts Go backend route bindings via pre-compiled regex matching.
pub fn extract_go_routes(file_path: &str, content: &str, omit_tests: bool) -> Vec<RouteBinding> {
    let mut routes = Vec::new();
    let line_index = LineIndex::new(content);
    for cap in BACKEND_ROUTE_REGEX.captures_iter(content) {
        if let (Some(m), Some(path_match), Some(handler_match)) =
            (cap.get(0), cap.get(2), cap.get(3))
        {
            let route_path = path_match.as_str().to_string();
            let handler = handler_match.as_str().to_string();
            let base_handler = handler.split('.').next_back().unwrap_or(&handler);
            if omit_tests && is_go_test_function(base_handler) {
                continue;
            }
            let line = line_index.line_number(m.start());

            routes.push(RouteBinding::new(
                route_path,
                handler,
                file_path.to_string(),
                line,
                true,
            ));
        }
    }
    routes
}

/// Phase-1 AST fact extractor for Go source files.
pub fn extract_go_file_facts(
    opts: &GoFactOptions<'_>,
    content: &str,
) -> Result<FileFacts, DistillError> {
    let mut parser = tree_sitter::Parser::new();
    let lang = tree_sitter_go::LANGUAGE.into();
    parser.set_language(&lang).map_err(|e| {
        DistillError::SymbolNotFound(format!("failed to initialize tree-sitter-go: {e}"))
    })?;

    let tree = parser.parse(content, None).ok_or_else(|| {
        DistillError::SymbolNotFound("failed to parse Go source code".to_string())
    })?;

    let root = tree.root_node();
    let mut ast_pkg = None;
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "package_clause" {
            if let Some(id) = child.child_by_field_name("package") {
                ast_pkg = Some(&content[id.byte_range()]);
            } else {
                let text = &content[child.byte_range()];
                if let Some(pkg) = text.strip_prefix("package") {
                    ast_pkg = Some(pkg.trim());
                }
            }
            break;
        }
    }

    let pkg_path = derive_go_package_path(opts, ast_pkg);
    let mut extractor = GoFactExtractor::new(opts, pkg_path.clone(), content);
    extractor.extract(root);

    let contracts = resolve_go_contracts(
        &extractor.file_path_str,
        &extractor.structs,
        &extractor.interfaces,
        &extractor.methods,
    );
    let routes = extract_go_routes(&extractor.file_path_str, content, opts.omit_tests);

    Ok(FileFacts {
        file_path: opts.file_path.to_path_buf(),
        rel_path: opts.rel_path.to_path_buf(),
        crate_or_pkg: pkg_path,
        symbols: extractor.symbols,
        contracts,
        routes,
        field_types: extractor.field_types,
        return_types: extractor.return_types,
        exports: Vec::new(),
        calls: extractor.calls,
    })
}

struct GoFactExtractor<'a> {
    file_path_str: String,
    pkg_path: String,
    content: &'a str,
    line_index: LineIndex,
    imports: HashMap<String, String>,
    symbols: Vec<SymbolDef>,
    structs: Vec<(String, usize)>,
    interfaces: Vec<RawInterface>,
    methods: Vec<GoMethodInfo>,
    field_types: HashMap<String, HashMap<String, TypeRef>>,
    return_types: HashMap<String, TypeRef>,
    calls: Vec<RawCallSite>,
    omit_tests: bool,
}

impl<'a> GoFactExtractor<'a> {
    fn new(opts: &GoFactOptions<'_>, pkg_path: String, content: &'a str) -> Self {
        Self {
            file_path_str: opts.file_path.to_string_lossy().to_string(),
            pkg_path,
            content,
            line_index: LineIndex::new(content),
            imports: HashMap::new(),
            symbols: Vec::new(),
            structs: Vec::new(),
            interfaces: Vec::new(),
            methods: Vec::new(),
            field_types: HashMap::new(),
            return_types: HashMap::new(),
            calls: Vec::new(),
            omit_tests: opts.omit_tests,
        }
    }

    fn extract(&mut self, root: tree_sitter::Node<'_>) {
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            match child.kind() {
                "import_declaration" => self.record_import_declaration(child),
                "type_declaration" => self.record_type_declaration(child),
                "function_declaration" => self.record_function_declaration(child),
                "method_declaration" => self.record_method_declaration(child),
                _ => {}
            }
        }
    }

    fn record_import_declaration(&mut self, node: tree_sitter::Node<'_>) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "import_spec" {
                self.record_import_spec(child);
            } else if child.kind() == "import_spec_list" {
                let mut inner_cursor = child.walk();
                for spec in child.children(&mut inner_cursor) {
                    if spec.kind() == "import_spec" {
                        self.record_import_spec(spec);
                    }
                }
            }
        }
    }

    fn record_import_spec(&mut self, node: tree_sitter::Node<'_>) {
        let path_node = node.child_by_field_name("path");
        let Some(path_node) = path_node else { return };
        let raw_path = &self.content[path_node.byte_range()];
        let clean_path = raw_path.trim_matches(|c| c == '"' || c == '`');
        if clean_path.is_empty() {
            return;
        }

        if let Some(name_node) = node.child_by_field_name("name") {
            let alias = self.content[name_node.byte_range()].trim();
            if alias != "_" && alias != "." && !alias.is_empty() {
                self.imports.insert(alias.to_string(), clean_path.to_string());
                return;
            }
        }

        let default_alias = clean_path.split('/').next_back().unwrap_or(clean_path);
        self.imports.insert(default_alias.to_string(), clean_path.to_string());
    }

    fn parse_go_type(&self, node: tree_sitter::Node<'_>) -> TypeRef {
        match node.kind() {
            "type_identifier" => {
                let name = &self.content[node.byte_range()];
                TypeRef::Known {
                    crate_or_pkg: self.pkg_path.clone(),
                    type_name: name.to_string(),
                }
            }
            "qualified_type" => {
                let pkg_node = node.child_by_field_name("package");
                let name_node = node.child_by_field_name("name");
                if let (Some(pn), Some(nn)) = (pkg_node, name_node) {
                    let pkg_alias = &self.content[pn.byte_range()];
                    let name = &self.content[nn.byte_range()];
                    let resolved_pkg = self
                        .imports
                        .get(pkg_alias)
                        .cloned()
                        .unwrap_or_else(|| pkg_alias.to_string());
                    TypeRef::Known {
                        crate_or_pkg: resolved_pkg,
                        type_name: name.to_string(),
                    }
                } else {
                    TypeRef::Unknown
                }
            }
            "pointer_type" => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() != "*" {
                        let inner = self.parse_go_type(child);
                        return TypeRef::Applied {
                            container: ContainerKind::Box,
                            inner: Box::new(inner),
                        };
                    }
                }
                TypeRef::Unknown
            }
            "slice_type" | "array_type" => {
                if let Some(elem) = node.child_by_field_name("element") {
                    let inner = self.parse_go_type(elem);
                    TypeRef::Applied {
                        container: ContainerKind::Vec,
                        inner: Box::new(inner),
                    }
                } else {
                    TypeRef::Unknown
                }
            }
            _ => TypeRef::Unknown,
        }
    }

    fn record_type_declaration(&mut self, node: tree_sitter::Node<'_>) {
        let mut cursor = node.walk();
        for spec in node.children(&mut cursor) {
            if spec.kind() == "type_spec" {
                self.record_type_spec(spec);
            }
        }
    }

    fn record_type_spec(&mut self, spec: tree_sitter::Node<'_>) {
        let name_node = spec.child_by_field_name("name");
        let Some(name_node) = name_node else { return };
        let name = self.content[name_node.byte_range()].to_string();
        let start_line = self.line_index.line_number(spec.start_byte());
        let end_line = self
            .line_index
            .line_number(spec.end_byte().saturating_sub(1))
            .max(start_line);
        let type_node = spec.child_by_field_name("type");
        let kind = type_node.map(|t| t.kind()).unwrap_or("type");

        match kind {
            "struct_type" => {
                let qualified_name = format!("{}.{name}", self.pkg_path);
                self.symbols.push(SymbolDef {
                    name: name.clone(),
                    kind: "struct".to_string(),
                    file: self.file_path_str.clone(),
                    line: start_line,
                    language: "go".to_string(),
                    end_line,
                    qualified_name,
                });
                self.structs.push((name.clone(), start_line));

                if let Some(tn) = type_node {
                    self.record_struct_fields(&name, tn);
                }
            }
            "interface_type" => {
                let qualified_name = format!("{}.{name}", self.pkg_path);
                self.symbols.push(SymbolDef {
                    name: name.clone(),
                    kind: "interface".to_string(),
                    file: self.file_path_str.clone(),
                    line: start_line,
                    language: "go".to_string(),
                    end_line,
                    qualified_name,
                });
                if let Some(tn) = type_node {
                    let req_methods = extract_interface_methods(tn, self.content);
                    self.interfaces.push(RawInterface {
                        name,
                        methods: req_methods,
                    });
                }
            }
            _ => {
                let qualified_name = format!("{}.{name}", self.pkg_path);
                self.symbols.push(SymbolDef {
                    name,
                    kind: "type".to_string(),
                    file: self.file_path_str.clone(),
                    line: start_line,
                    language: "go".to_string(),
                    end_line,
                    qualified_name,
                });
            }
        }
    }

    fn record_struct_fields(&mut self, struct_name: &str, struct_node: tree_sitter::Node<'_>) {
        let mut cursor = struct_node.walk();
        let mut fields_map = HashMap::new();

        for child in struct_node.children(&mut cursor) {
            if child.kind() == "field_declaration_list" {
                let mut list_cursor = child.walk();
                for field_decl in child.children(&mut list_cursor) {
                    if field_decl.kind() == "field_declaration" {
                        self.extract_field_decl(field_decl, &mut fields_map);
                    }
                }
            }
        }

        if !fields_map.is_empty() {
            self.field_types.insert(struct_name.to_string(), fields_map);
        }
    }

    fn extract_field_decl(
        &self,
        decl: tree_sitter::Node<'_>,
        fields_map: &mut HashMap<String, TypeRef>,
    ) {
        let type_node = decl.child_by_field_name("type");
        let Some(type_node) = type_node else {
            let text = self.content[decl.byte_range()].trim();
            let field_name = text.split('.').next_back().unwrap_or(text).trim_start_matches('*');
            if !field_name.is_empty() {
                let ty_ref = self.parse_go_type(decl);
                fields_map.insert(field_name.to_string(), ty_ref);
            }
            return;
        };

        let ty_ref = self.parse_go_type(type_node);
        let mut cursor = decl.walk();
        let mut found_name = false;

        for child in decl.children(&mut cursor) {
            if child.kind() == "field_identifier" {
                found_name = true;
                let field_name = &self.content[child.byte_range()];
                fields_map.insert(field_name.to_string(), ty_ref.clone());
            }
        }

        if !found_name {
            let text = &self.content[type_node.byte_range()];
            let field_name = text.split('.').next_back().unwrap_or(text).trim_start_matches('*');
            if !field_name.is_empty() {
                fields_map.insert(field_name.to_string(), ty_ref);
            }
        }
    }

    fn record_function_declaration(&mut self, node: tree_sitter::Node<'_>) {
        let name_node = node.child_by_field_name("name");
        let Some(name_node) = name_node else { return };
        let name = self.content[name_node.byte_range()].to_string();

        let sig_end = node
            .child_by_field_name("body")
            .map(|b| b.start_byte())
            .unwrap_or_else(|| node.end_byte());
        let sig = self.content[node.start_byte()..sig_end].trim_end();

        if self.omit_tests && (is_go_test_function(&name) || has_go_test_signature(sig)) {
            return;
        }

        let start_line = self.line_index.line_number(node.start_byte());
        let end_line = self
            .line_index
            .line_number(node.end_byte().saturating_sub(1))
            .max(start_line);
        let qualified_name = format!("{}.{name}", self.pkg_path);

        self.symbols.push(SymbolDef {
            name,
            kind: "fn".to_string(),
            file: self.file_path_str.clone(),
            line: start_line,
            language: "go".to_string(),
            end_line,
            qualified_name: qualified_name.clone(),
        });

        let mut local_vars = HashMap::new();
        if let Some(params) = node.child_by_field_name("parameters") {
            self.collect_parameters(params, &mut local_vars);
        }

        if let Some(res) = node.child_by_field_name("result") {
            let ret_ty = self.parse_result_type(res);
            if ret_ty != TypeRef::Unknown {
                self.return_types.insert(qualified_name.clone(), ret_ty);
            }
        }

        if let Some(body) = node.child_by_field_name("body") {
            self.walk_body_node(body, &qualified_name, &mut local_vars);
        }
    }

    fn record_method_declaration(&mut self, node: tree_sitter::Node<'_>) {
        let name_node = node.child_by_field_name("name");
        let recv_node = node.child_by_field_name("receiver");
        let (Some(name_node), Some(recv_node)) = (name_node, recv_node) else { return };

        let name = self.content[name_node.byte_range()].to_string();
        let (recv_var, recv_type, is_pointer) = self.parse_receiver(recv_node);
        if recv_type.is_empty() {
            return;
        }

        let sig_end = node
            .child_by_field_name("body")
            .map(|b| b.start_byte())
            .unwrap_or_else(|| node.end_byte());
        let sig = self.content[node.start_byte()..sig_end].trim_end();

        if self.omit_tests && (is_go_test_function(&name) || has_go_test_signature(sig)) {
            return;
        }

        let start_line = self.line_index.line_number(node.start_byte());
        let end_line = self
            .line_index
            .line_number(node.end_byte().saturating_sub(1))
            .max(start_line);
        let qualified_name = format!("{}.{recv_type}.{name}", self.pkg_path);

        self.symbols.push(SymbolDef {
            name: name.clone(),
            kind: "fn".to_string(),
            file: self.file_path_str.clone(),
            line: start_line,
            language: "go".to_string(),
            end_line,
            qualified_name: qualified_name.clone(),
        });

        self.methods.push(GoMethodInfo {
            receiver_type: recv_type.clone(),
            is_pointer,
            method_name: name,
            signature: sig.to_string(),
        });

        let mut local_vars = HashMap::new();
        let rec_ty = TypeRef::Known {
            crate_or_pkg: self.pkg_path.clone(),
            type_name: recv_type,
        };
        if let Some(rv) = recv_var {
            local_vars.insert(rv, rec_ty);
        }

        if let Some(params) = node.child_by_field_name("parameters") {
            self.collect_parameters(params, &mut local_vars);
        }

        if let Some(res) = node.child_by_field_name("result") {
            let ret_ty = self.parse_result_type(res);
            if ret_ty != TypeRef::Unknown {
                self.return_types.insert(qualified_name.clone(), ret_ty);
            }
        }

        if let Some(body) = node.child_by_field_name("body") {
            self.walk_body_node(body, &qualified_name, &mut local_vars);
        }
    }

    fn parse_receiver(&self, recv_node: tree_sitter::Node<'_>) -> (Option<String>, String, bool) {
        let text = self.content[recv_node.byte_range()].trim();
        let is_pointer = text.contains('*');

        let mut recv_var = None;
        let mut recv_type = String::new();

        let mut cursor = recv_node.walk();
        for child in recv_node.children(&mut cursor) {
            if child.kind() == "parameter_declaration" {
                let mut p_cursor = child.walk();
                for p_child in child.children(&mut p_cursor) {
                    if p_child.kind() == "identifier" {
                        recv_var = Some(self.content[p_child.byte_range()].to_string());
                    } else if p_child.kind() == "type_identifier" {
                        recv_type = self.content[p_child.byte_range()].to_string();
                    } else if p_child.kind() == "pointer_type" {
                        let mut pt_cursor = p_child.walk();
                        for pt_child in p_child.children(&mut pt_cursor) {
                            if pt_child.kind() == "type_identifier" {
                                recv_type = self.content[pt_child.byte_range()].to_string();
                            }
                        }
                    }
                }
            }
        }

        if recv_type.is_empty() {
            recv_type = text
                .trim_matches(|c| c == '(' || c == ')' || c == ' ')
                .split_whitespace()
                .last()
                .unwrap_or("")
                .trim_start_matches('*')
                .to_string();
        }

        (recv_var, recv_type, is_pointer)
    }

    fn collect_parameters(
        &self,
        params: tree_sitter::Node<'_>,
        local_vars: &mut HashMap<String, TypeRef>,
    ) {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() == "parameter_declaration" {
                let mut names = Vec::new();
                let mut type_node = None;
                let mut p_cursor = child.walk();
                for p_child in child.children(&mut p_cursor) {
                    if p_child.kind() == "identifier" {
                        names.push(&self.content[p_child.byte_range()]);
                    } else if p_child.kind() != "," && p_child.kind() != "..." {
                        type_node = Some(p_child);
                    }
                }
                if let Some(tn) = type_node {
                    let ty_ref = self.parse_go_type(tn);
                    for name in names {
                        local_vars.insert(name.to_string(), ty_ref.clone());
                    }
                }
            }
        }
    }

    fn parse_result_type(&self, res_node: tree_sitter::Node<'_>) -> TypeRef {
        if res_node.kind() == "parameter_list" {
            let mut cursor = res_node.walk();
            for child in res_node.children(&mut cursor) {
                if child.kind() == "parameter_declaration" {
                    let mut p_cursor = child.walk();
                    for p_child in child.children(&mut p_cursor) {
                        if p_child.kind() != "identifier" && p_child.kind() != "," {
                            let ty_ref = self.parse_go_type(p_child);
                            if ty_ref != TypeRef::Unknown {
                                return ty_ref;
                            }
                        }
                    }
                }
            }
        }
        self.parse_go_type(res_node)
    }

    fn walk_body_node(
        &mut self,
        node: tree_sitter::Node<'_>,
        caller_fqn: &str,
        local_vars: &mut HashMap<String, TypeRef>,
    ) {
        match node.kind() {
            "short_var_declaration" => self.handle_short_var_declaration(node, caller_fqn, local_vars),
            "var_declaration" => self.handle_var_declaration(node, caller_fqn, local_vars),
            "call_expression" => {
                self.handle_call_expression(node, caller_fqn, local_vars);
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    self.walk_body_node(child, caller_fqn, local_vars);
                }
            }
            "func_lit" => {
                let mut inner_vars = local_vars.clone();
                if let Some(params) = node.child_by_field_name("parameters") {
                    self.collect_parameters(params, &mut inner_vars);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    self.walk_body_node(body, caller_fqn, &mut inner_vars);
                }
            }
            _ => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    self.walk_body_node(child, caller_fqn, local_vars);
                }
            }
        }
    }

    fn handle_short_var_declaration(
        &mut self,
        node: tree_sitter::Node<'_>,
        caller_fqn: &str,
        local_vars: &mut HashMap<String, TypeRef>,
    ) {
        let left = node.child_by_field_name("left");
        let right = node.child_by_field_name("right");
        if let (Some(left), Some(right)) = (left, right) {
            let mut var_names = Vec::new();
            let mut l_cursor = left.walk();
            for l_child in left.children(&mut l_cursor) {
                if l_child.kind() == "identifier" {
                    var_names.push(self.content[l_child.byte_range()].to_string());
                }
            }

            let mut right_exprs = Vec::new();
            let mut r_cursor = right.walk();
            for r_child in right.children(&mut r_cursor) {
                if r_child.kind() != "," {
                    right_exprs.push(r_child);
                }
            }

            for (idx, name) in var_names.into_iter().enumerate() {
                if let Some(&expr) = right_exprs.get(idx) {
                    let ty_ref = self.infer_expr_type(expr, local_vars);
                    if ty_ref != TypeRef::Unknown {
                        local_vars.insert(name, ty_ref);
                    }
                }
            }

            for expr in right_exprs {
                self.walk_body_node(expr, caller_fqn, local_vars);
            }
        }
    }

    fn handle_var_declaration(
        &mut self,
        node: tree_sitter::Node<'_>,
        caller_fqn: &str,
        local_vars: &mut HashMap<String, TypeRef>,
    ) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "var_spec" {
                let mut var_names = Vec::new();
                let mut type_node = None;
                let mut values = Vec::new();
                let mut s_cursor = child.walk();
                for s_child in child.children(&mut s_cursor) {
                    if s_child.kind() == "identifier" {
                        var_names.push(self.content[s_child.byte_range()].to_string());
                    } else if s_child.kind() == "expression_list" {
                        let mut v_cursor = s_child.walk();
                        for v in s_child.children(&mut v_cursor) {
                            if v.kind() != "," {
                                values.push(v);
                            }
                        }
                    } else if s_child.kind() != "," && s_child.kind() != "=" {
                        type_node = Some(s_child);
                    }
                }

                let ty_ref = type_node.map(|tn| self.parse_go_type(tn)).unwrap_or(TypeRef::Unknown);
                for (idx, name) in var_names.into_iter().enumerate() {
                    let resolved_ty = if ty_ref != TypeRef::Unknown {
                        ty_ref.clone()
                    } else if let Some(&val) = values.get(idx) {
                        self.infer_expr_type(val, local_vars)
                    } else {
                        TypeRef::Unknown
                    };
                    if resolved_ty != TypeRef::Unknown {
                        local_vars.insert(name, resolved_ty);
                    }
                }

                for v in values {
                    self.walk_body_node(v, caller_fqn, local_vars);
                }
            }
        }
    }

    fn infer_expr_type(
        &self,
        expr: tree_sitter::Node<'_>,
        local_vars: &HashMap<String, TypeRef>,
    ) -> TypeRef {
        match expr.kind() {
            "unary_expression" => {
                if let Some(operand) = expr.child_by_field_name("operand") {
                    self.infer_expr_type(operand, local_vars)
                } else {
                    TypeRef::Unknown
                }
            }
            "composite_literal" => {
                if let Some(tn) = expr.child_by_field_name("type") {
                    self.parse_go_type(tn)
                } else {
                    TypeRef::Unknown
                }
            }
            "call_expression" => {
                if let Some(func) = expr.child_by_field_name("function") {
                    self.infer_call_return_type(func)
                } else {
                    TypeRef::Unknown
                }
            }
            "identifier" => {
                let name = &self.content[expr.byte_range()];
                local_vars.get(name).cloned().unwrap_or(TypeRef::Unknown)
            }
            _ => TypeRef::Unknown,
        }
    }

    fn infer_call_return_type(&self, func: tree_sitter::Node<'_>) -> TypeRef {
        if func.kind() == "identifier" {
            let name = &self.content[func.byte_range()];
            if name.starts_with("New") && name.len() > 3 {
                let type_name = &name[3..];
                return TypeRef::Known {
                    crate_or_pkg: self.pkg_path.clone(),
                    type_name: type_name.to_string(),
                };
            }
        } else if func.kind() == "selector_expression" {
            let op = func.child_by_field_name("operand");
            let f = func.child_by_field_name("field");
            if let (Some(op), Some(f)) = (op, f) {
                let op_name = &self.content[op.byte_range()];
                let f_name = &self.content[f.byte_range()];
                if f_name.starts_with("New") && f_name.len() > 3 {
                    let type_name = &f_name[3..];
                    let resolved_pkg = self
                        .imports
                        .get(op_name)
                        .cloned()
                        .unwrap_or_else(|| op_name.to_string());
                    return TypeRef::Known {
                        crate_or_pkg: resolved_pkg,
                        type_name: type_name.to_string(),
                    };
                }
            }
        }
        TypeRef::Unknown
    }

    fn handle_call_expression(
        &mut self,
        node: tree_sitter::Node<'_>,
        caller_fqn: &str,
        local_vars: &HashMap<String, TypeRef>,
    ) {
        let Some(func) = node.child_by_field_name("function") else { return };
        let call_line = self.line_index.line_number(node.start_byte()) as u32;

        if func.kind() == "selector_expression" {
            self.handle_selector_call(func, caller_fqn, call_line, local_vars);
        } else if func.kind() == "identifier" {
            let fn_name = &self.content[func.byte_range()];
            let target = RawCalleeTarget::Path(format!("{}.{fn_name}", self.pkg_path));
            self.calls.push(RawCallSite {
                caller_fqn: caller_fqn.to_string(),
                call_line,
                target,
            });
        }
    }

    fn handle_selector_call(
        &mut self,
        func: tree_sitter::Node<'_>,
        caller_fqn: &str,
        call_line: u32,
        local_vars: &HashMap<String, TypeRef>,
    ) {
        let op = func.child_by_field_name("operand");
        let field = func.child_by_field_name("field");
        let (Some(op), Some(field)) = (op, field) else { return };
        let field_name = &self.content[field.byte_range()];

        let target = if op.kind() == "identifier" {
            let op_name = &self.content[op.byte_range()];
            if let Some(import_path) = self.imports.get(op_name) {
                RawCalleeTarget::Path(format!("{import_path}.{field_name}"))
            } else if let Some(ty_ref) = local_vars.get(op_name) {
                RawCalleeTarget::Method {
                    receiver: ty_ref.unwrap_container().clone(),
                    method_name: field_name.to_string(),
                }
            } else {
                RawCalleeTarget::Method {
                    receiver: TypeRef::Unknown,
                    method_name: field_name.to_string(),
                }
            }
        } else if op.kind() == "selector_expression" {
            let (base_name, fields) = decompose_selector_chain(op, self.content);
            if let Some(ty_ref) = local_vars.get(&base_name) {
                if fields.len() <= 4 {
                    RawCalleeTarget::FieldChain {
                        receiver: ty_ref.unwrap_container().clone(),
                        fields,
                        method_name: field_name.to_string(),
                    }
                } else {
                    RawCalleeTarget::Method {
                        receiver: TypeRef::Unknown,
                        method_name: field_name.to_string(),
                    }
                }
            } else if let Some(import_path) = self.imports.get(&base_name) {
                RawCalleeTarget::Path(format!("{import_path}.{}.{field_name}", fields.join(".")))
            } else {
                RawCalleeTarget::FieldChain {
                    receiver: TypeRef::Unknown,
                    fields,
                    method_name: field_name.to_string(),
                }
            }
        } else {
            RawCalleeTarget::Method {
                receiver: TypeRef::Unknown,
                method_name: field_name.to_string(),
            }
        };

        self.calls.push(RawCallSite {
            caller_fqn: caller_fqn.to_string(),
            call_line,
            target,
        });
    }
}

fn decompose_selector_chain(
    node: tree_sitter::Node<'_>,
    content: &str,
) -> (String, Vec<String>) {
    let mut current = node;
    let mut fields = Vec::new();

    while current.kind() == "selector_expression" {
        if let Some(f) = current.child_by_field_name("field") {
            fields.push(content[f.byte_range()].to_string());
        }
        if let Some(op) = current.child_by_field_name("operand") {
            current = op;
        } else {
            break;
        }
    }

    fields.reverse();
    let base = content[current.byte_range()].trim().to_string();
    (base, fields)
}

fn extract_interface_methods(node: tree_sitter::Node<'_>, content: &str) -> Vec<(String, String)> {
    let mut methods = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "method_elem" || child.kind() == "method_spec" {
            let sig = content[child.byte_range()].trim();
            if let Some(paren) = sig.find('(') {
                let m_name = sig[..paren].trim().to_string();
                if !m_name.is_empty() && !m_name.starts_with('/') {
                    methods.push((m_name, sig.to_string()));
                }
            }
        }
    }
    methods
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_go_symbols_and_spans() {
        let code = r#"package service

type Status string

type User struct {
    ID    string `json:"id"`
    Email string `json:"email"`
}

type UserRepository interface {
    GetUser(id string) (*User, error)
}

type UserService struct {
    repo UserRepository
}

func NewUserService(repo UserRepository) *UserService {
    return &UserService{repo: repo}
}

func (s *UserService) GetUser(id string) (*User, error) {
    return s.repo.GetUser(id)
}
"#;
        let p = Path::new("pkg/service/user.go");
        let opts = GoFactOptions {
            file_path: p,
            rel_path: p,
            package_path: Some("github.com/acme/project/pkg/service"),
            omit_tests: true,
        };

        let facts = extract_go_file_facts(&opts, code).unwrap();
        assert_eq!(facts.crate_or_pkg, "github.com/acme/project/pkg/service");

        let sym_names: Vec<&str> = facts.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(sym_names.contains(&"Status"));
        assert!(sym_names.contains(&"User"));
        assert!(sym_names.contains(&"UserRepository"));
        assert!(sym_names.contains(&"UserService"));
        assert!(sym_names.contains(&"NewUserService"));
        assert!(sym_names.contains(&"GetUser"));

        let get_user = facts.symbols.iter().find(|s| s.name == "GetUser").unwrap();
        assert_eq!(
            get_user.qualified_name,
            "github.com/acme/project/pkg/service.UserService.GetUser"
        );
        assert_eq!(get_user.kind, "fn");
        assert_eq!(get_user.language, "go");
        assert!(get_user.line > 0 && get_user.end_line >= get_user.line);
    }

    #[test]
    fn test_extract_go_imports_and_calls() {
        let code = r#"package handler

import (
    "fmt"
    custom "github.com/acme/project/pkg/util"
)

type Handler struct {
    store Store
}

func (h *Handler) Handle() {
    fmt.Println("start")
    custom.DoWork()
    svc := &Service{}
    svc.Process()
    h.store.users.Get()
}
"#;
        let p = Path::new("internal/handler/handler.go");
        let opts = GoFactOptions {
            file_path: p,
            rel_path: p,
            package_path: Some("github.com/acme/project/internal/handler"),
            omit_tests: true,
        };

        let facts = extract_go_file_facts(&opts, code).unwrap();
        assert_eq!(facts.calls.len(), 4);

        // Call 1: fmt.Println
        assert_eq!(facts.calls[0].caller_fqn, "github.com/acme/project/internal/handler.Handler.Handle");
        assert_eq!(facts.calls[0].target, RawCalleeTarget::Path("fmt.Println".to_string()));

        // Call 2: custom.DoWork
        assert_eq!(
            facts.calls[1].target,
            RawCalleeTarget::Path("github.com/acme/project/pkg/util.DoWork".to_string())
        );

        // Call 3: svc.Process (from svc := &Service{})
        assert_eq!(
            facts.calls[2].target,
            RawCalleeTarget::Method {
                receiver: TypeRef::Known {
                    crate_or_pkg: "github.com/acme/project/internal/handler".to_string(),
                    type_name: "Service".to_string(),
                },
                method_name: "Process".to_string(),
            }
        );

        // Call 4: h.store.users.Get
        assert_eq!(
            facts.calls[3].target,
            RawCalleeTarget::FieldChain {
                receiver: TypeRef::Known {
                    crate_or_pkg: "github.com/acme/project/internal/handler".to_string(),
                    type_name: "Handler".to_string(),
                },
                fields: vec!["store".to_string(), "users".to_string()],
                method_name: "Get".to_string(),
            }
        );
    }

    #[test]
    fn test_extract_go_routes() {
        let code = r#"package main

func Register(r *gin.Engine, api *chi.Mux) {
    r.GET("/api/v1/users/:id", s.GetUser)
    r.POST("/api/v1/users", s.CreateUser)
    api.Get("/health", HealthHandler)
    http.HandleFunc("/metrics", MetricsHandler)
}
"#;
        let routes = extract_go_routes("main.go", code, false);
        assert_eq!(routes.len(), 4);
        assert_eq!(routes[0].route_path, "/api/v1/users/:id");
        assert_eq!(routes[0].handler_or_caller, "s.GetUser");
        assert_eq!(routes[1].route_path, "/api/v1/users");
        assert_eq!(routes[2].route_path, "/health");
        assert_eq!(routes[3].route_path, "/metrics");
        assert!(routes.iter().all(|r| r.is_backend));
    }

    #[test]
    fn test_extract_go_omit_tests() {
        let code = r#"package math

func Add(a, b int) int {
    return a + b
}

func TestAdd(t *testing.T) {
    if Add(1, 2) != 3 {
        t.Fail()
    }
}
"#;
        let p = Path::new("math.go");
        let opts = GoFactOptions {
            file_path: p,
            rel_path: p,
            package_path: None,
            omit_tests: true,
        };

        let facts = extract_go_file_facts(&opts, code).unwrap();
        assert_eq!(facts.symbols.len(), 1);
        assert_eq!(facts.symbols[0].name, "Add");
    }
}
