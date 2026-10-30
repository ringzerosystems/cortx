//! Phase-1 TypeScript, TSX, and JavaScript AST fact, barrel, and route extractor.

use crate::error::DistillError;
use crate::indexer::rust_graph::{ExportEntry, FileFacts, RawCallSite, RawCalleeTarget, TypeRef};
use crate::indexer::{RouteBinding, SymbolDef};
use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_parser::Parser;
use oxc_span::SourceType;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

static CLIENT_ROUTE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:fetch|invoke|axios\s*\.\s*(?:get|post|put|delete|patch))\s*\(\s*["'`]([^"'`]+)["'`]"#)
        .expect("valid client route regex")
});

/// Configuration options for Phase-1 TypeScript / JS AST fact extraction.
#[derive(Debug, Clone)]
pub struct TsFactOptions<'a> {
    pub file_path: &'a Path,
    pub rel_path: &'a Path,
    pub package_name: Option<&'a str>,
    pub omit_tests: bool,
}

/// Derives the workspace-relative module identifier without extension, using `/` separators.
pub fn normalize_ts_module_path(rel_path: &Path) -> String {
    let s = rel_path.to_string_lossy().replace('\\', "/");
    let trimmed = s.strip_prefix("./").unwrap_or(&s);
    for ext in [
        ".d.ts", ".d.mts", ".d.cts", ".tsx", ".jsx", ".ts", ".js", ".mjs", ".cjs",
    ] {
        if let Some(stripped) = trimmed.strip_suffix(ext) {
            return stripped.to_string();
        }
    }
    trimmed.to_string()
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

#[derive(Debug, Clone)]
struct ImportBinding {
    specifier: String,
    imported_name: String,
}

/// Phase-1 AST fact extractor for TypeScript, TSX, and JavaScript source files.
pub fn extract_ts_file_facts(
    opts: &TsFactOptions<'_>,
    content: &str,
) -> Result<FileFacts, DistillError> {
    let allocator = Allocator::default();
    let is_jsx = opts
        .file_path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e == "tsx" || e == "jsx");

    let source_type = SourceType::default()
        .with_typescript(true)
        .with_jsx(is_jsx)
        .with_module(true);

    let ret = Parser::new(&allocator, content, source_type).parse();
    if !ret.errors.is_empty() && ret.program.body.is_empty() {
        return Err(DistillError::SymbolNotFound(
            "Failed to parse TypeScript/TSX syntax tree".to_string(),
        ));
    }

    let rel_module = normalize_ts_module_path(opts.rel_path);
    let mut extractor = TsFactExtractor::new(opts, rel_module, content);
    extractor.extract(&ret.program);

    Ok(FileFacts {
        file_path: opts.file_path.to_path_buf(),
        rel_path: opts.rel_path.to_path_buf(),
        crate_or_pkg: opts
            .package_name
            .map(|s| s.to_string())
            .unwrap_or_else(|| extractor.rel_module.clone()),
        symbols: extractor.symbols,
        contracts: Vec::new(),
        routes: extractor.routes,
        field_types: extractor.field_types,
        return_types: extractor.return_types,
        exports: extractor.exports,
        calls: extractor.calls,
    })
}

struct TsFactExtractor<'a> {
    file_path_str: String,
    rel_module: String,
    line_index: LineIndex,
    content: &'a str,
    imports: HashMap<String, ImportBinding>,
    local_declarations: HashSet<String>,
    symbols: Vec<SymbolDef>,
    exports: Vec<ExportEntry>,
    calls: Vec<RawCallSite>,
    routes: Vec<RouteBinding>,
    field_types: HashMap<String, HashMap<String, TypeRef>>,
    return_types: HashMap<String, TypeRef>,
    current_class: Option<String>,
    omit_tests: bool,
}

impl<'a> TsFactExtractor<'a> {
    fn new(opts: &TsFactOptions<'_>, rel_module: String, content: &'a str) -> Self {
        Self {
            file_path_str: opts.file_path.to_string_lossy().to_string(),
            rel_module,
            line_index: LineIndex::new(content),
            content,
            imports: HashMap::new(),
            local_declarations: HashSet::new(),
            symbols: Vec::new(),
            exports: Vec::new(),
            calls: Vec::new(),
            routes: Vec::new(),
            field_types: HashMap::new(),
            return_types: HashMap::new(),
            current_class: None,
            omit_tests: opts.omit_tests,
        }
    }

    fn extract(&mut self, program: &Program<'_>) {
        for stmt in &program.body {
            collect_declaration_names(stmt, &mut self.local_declarations);
        }
        for stmt in &program.body {
            self.walk_top_level_statement(stmt);
        }
        self.extract_routes();
    }

    fn walk_top_level_statement(&mut self, stmt: &Statement<'_>) {
        let module_caller = format!("{}::<module>", self.rel_module);
        match stmt {
            Statement::ImportDeclaration(decl) => {
                self.record_import_declaration(decl);
            }
            Statement::ExportAllDeclaration(decl) => {
                self.exports.push(ExportEntry::StarReExport {
                    specifier: decl.source.value.to_string(),
                });
            }
            Statement::ExportNamedDeclaration(decl) => {
                self.walk_export_named(decl);
            }
            Statement::ExportDefaultDeclaration(decl) => {
                self.walk_export_default(decl);
            }
            Statement::FunctionDeclaration(func) => {
                self.walk_function(func);
            }
            Statement::ClassDeclaration(cls) => {
                self.walk_class(cls);
            }
            Statement::VariableDeclaration(var) => {
                self.walk_variable_declaration(var, &module_caller);
            }
            Statement::TSInterfaceDeclaration(decl) => {
                self.record_interface(decl);
            }
            Statement::TSTypeAliasDeclaration(decl) => {
                self.record_type_alias(decl);
            }
            Statement::TSEnumDeclaration(decl) => {
                self.record_enum(decl);
            }
            other => {
                self.walk_statement(other, &module_caller);
            }
        }
    }

    fn record_import_declaration(&mut self, decl: &ImportDeclaration<'_>) {
        let specifier = decl.source.value.to_string();
        if let Some(ref specifiers) = decl.specifiers {
            for spec in specifiers {
                match spec {
                    ImportDeclarationSpecifier::ImportSpecifier(s) => {
                        self.imports.insert(
                            s.local.name.to_string(),
                            ImportBinding {
                                specifier: specifier.clone(),
                                imported_name: s.imported.name().to_string(),
                            },
                        );
                    }
                    ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                        self.imports.insert(
                            s.local.name.to_string(),
                            ImportBinding {
                                specifier: specifier.clone(),
                                imported_name: "default".to_string(),
                            },
                        );
                    }
                    ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                        self.imports.insert(
                            s.local.name.to_string(),
                            ImportBinding {
                                specifier: specifier.clone(),
                                imported_name: "*".to_string(),
                            },
                        );
                    }
                }
            }
        }
    }

    fn walk_export_named(&mut self, decl: &ExportNamedDeclaration<'_>) {
        let module_caller = format!("{}::<module>", self.rel_module);
        if let Some(ref src_lit) = decl.source {
            let specifier = src_lit.value.to_string();
            for spec in &decl.specifiers {
                self.exports.push(ExportEntry::NamedReExport {
                    specifier: specifier.clone(),
                    imported: spec.local.name().to_string(),
                    exported: spec.exported.name().to_string(),
                });
            }
        } else {
            for spec in &decl.specifiers {
                self.exports
                    .push(ExportEntry::Local(spec.exported.name().to_string()));
            }
            if let Some(ref d) = decl.declaration {
                self.walk_export_declaration(d, &module_caller);
            }
        }
    }

    fn walk_export_declaration(&mut self, d: &Declaration<'_>, caller: &str) {
        match d {
            Declaration::FunctionDeclaration(func) => {
                if let Some(ref id) = func.id {
                    self.exports.push(ExportEntry::Local(id.name.to_string()));
                }
                self.walk_function(func);
            }
            Declaration::ClassDeclaration(cls) => {
                if let Some(ref id) = cls.id {
                    self.exports.push(ExportEntry::Local(id.name.to_string()));
                }
                self.walk_class(cls);
            }
            Declaration::VariableDeclaration(var) => {
                for v in &var.declarations {
                    if let Some(name) = v.id.get_identifier_name() {
                        self.exports.push(ExportEntry::Local(name.to_string()));
                    }
                }
                self.walk_variable_declaration(var, caller);
            }
            Declaration::TSInterfaceDeclaration(iface) => {
                self.exports
                    .push(ExportEntry::Local(iface.id.name.to_string()));
                self.record_interface(iface);
            }
            Declaration::TSTypeAliasDeclaration(alias) => {
                self.exports
                    .push(ExportEntry::Local(alias.id.name.to_string()));
                self.record_type_alias(alias);
            }
            Declaration::TSEnumDeclaration(enum_decl) => {
                self.exports
                    .push(ExportEntry::Local(enum_decl.id.name.to_string()));
                self.record_enum(enum_decl);
            }
            _ => {}
        }
    }

    fn walk_export_default(&mut self, decl: &ExportDefaultDeclaration<'_>) {
        self.exports
            .push(ExportEntry::Local("default".to_string()));
        let module_caller = format!("{}::<module>", self.rel_module);
        match &decl.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(func) => {
                self.walk_function(func);
            }
            ExportDefaultDeclarationKind::ClassDeclaration(cls) => {
                self.walk_class(cls);
            }
            ExportDefaultDeclarationKind::TSInterfaceDeclaration(iface) => {
                self.record_interface(iface);
            }
            other => {
                if let Some(expr) = other.as_expression() {
                    self.walk_expression(expr, &module_caller);
                }
            }
        }
    }

    fn walk_function(&mut self, func: &Function<'_>) {
        let name = func
            .id
            .as_ref()
            .map(|id| id.name.as_str())
            .unwrap_or("default");

        if self.omit_tests && (name.starts_with("test_") || name == "test" || name == "it") {
            return;
        }

        let start_line = self.line_index.line_number(func.span.start as usize);
        let end_line = self
            .line_index
            .line_number(func.span.end.saturating_sub(1) as usize)
            .max(start_line);
        let qualified_name = format!("{}::{}", self.rel_module, name);

        self.symbols.push(SymbolDef {
            name: name.to_string(),
            kind: "fn".to_string(),
            file: self.file_path_str.clone(),
            line: start_line,
            language: "typescript".to_string(),
            end_line,
            qualified_name: qualified_name.clone(),
        });

        if let Some(ref ret_ann) = func.return_type {
            if let Some(ret_type) = extract_type_name_from_annotation(ret_ann) {
                self.return_types.insert(
                    qualified_name.clone(),
                    TypeRef::Known {
                        crate_or_pkg: "".to_string(),
                        type_name: ret_type,
                    },
                );
            }
        }

        if let Some(ref body) = func.body {
            for stmt in &body.statements {
                self.walk_statement(stmt, &qualified_name);
            }
        }
    }

    fn walk_class(&mut self, cls: &Class<'_>) {
        let cls_name = cls
            .id
            .as_ref()
            .map(|id| id.name.as_str())
            .unwrap_or("default");

        let start_line = self.line_index.line_number(cls.span.start as usize);
        let end_line = self
            .line_index
            .line_number(cls.span.end.saturating_sub(1) as usize)
            .max(start_line);
        let cls_fqn = format!("{}::{}", self.rel_module, cls_name);

        self.symbols.push(SymbolDef {
            name: cls_name.to_string(),
            kind: "class".to_string(),
            file: self.file_path_str.clone(),
            line: start_line,
            language: "typescript".to_string(),
            end_line,
            qualified_name: cls_fqn,
        });

        let prev_class = self.current_class.replace(cls_name.to_string());

        for elem in &cls.body.body {
            match elem {
                ClassElement::MethodDefinition(method) => {
                    self.walk_method(cls_name, method);
                }
                ClassElement::PropertyDefinition(prop) => {
                    self.walk_property(cls_name, prop);
                }
                ClassElement::StaticBlock(block) => {
                    let block_caller = format!("{}::{}", self.rel_module, cls_name);
                    for stmt in &block.body {
                        self.walk_statement(stmt, &block_caller);
                    }
                }
                _ => {}
            }
        }

        self.current_class = prev_class;
    }

    fn walk_method(&mut self, cls_name: &str, method: &MethodDefinition<'_>) {
        let m_name = method
            .key
            .name()
            .unwrap_or(std::borrow::Cow::Borrowed("method"));
        let m_start = self.line_index.line_number(method.span.start as usize);
        let m_end = self
            .line_index
            .line_number(method.span.end.saturating_sub(1) as usize)
            .max(m_start);
        let m_fqn = format!("{}::{}.{}", self.rel_module, cls_name, m_name);

        self.symbols.push(SymbolDef {
            name: m_name.to_string(),
            kind: "fn".to_string(),
            file: self.file_path_str.clone(),
            line: m_start,
            language: "typescript".to_string(),
            end_line: m_end,
            qualified_name: m_fqn.clone(),
        });

        if let Some(ref ret_ann) = method.value.return_type {
            if let Some(ret_type) = extract_type_name_from_annotation(ret_ann) {
                self.return_types.insert(
                    m_fqn.clone(),
                    TypeRef::Known {
                        crate_or_pkg: "".to_string(),
                        type_name: ret_type,
                    },
                );
            }
        }

        if let Some(ref body) = method.value.body {
            for stmt in &body.statements {
                self.walk_statement(stmt, &m_fqn);
            }
        }
    }

    fn walk_property(&mut self, cls_name: &str, prop: &PropertyDefinition<'_>) {
        if let (Some(name), Some(type_ann)) = (prop.key.name(), &prop.type_annotation) {
            if let Some(type_name) = extract_type_name_from_annotation(type_ann) {
                self.field_types
                    .entry(cls_name.to_string())
                    .or_default()
                    .insert(
                        name.to_string(),
                        TypeRef::Known {
                            crate_or_pkg: "".to_string(),
                            type_name,
                        },
                    );
            }
        }
        if let Some(ref init) = prop.value {
            let prop_caller = format!("{}::{}", self.rel_module, cls_name);
            self.walk_expression(init, &prop_caller);
        }
    }

    fn walk_variable_declaration(&mut self, var: &VariableDeclaration<'_>, caller: &str) {
        for decl in &var.declarations {
            let Some(name) = decl.id.get_identifier_name() else {
                if let Some(ref init) = decl.init {
                    self.walk_expression(init, caller);
                }
                continue;
            };

            let start_line = self.line_index.line_number(decl.span.start as usize);
            let end_line = self
                .line_index
                .line_number(decl.span.end.saturating_sub(1) as usize)
                .max(start_line);

            let is_arrow_or_fn = matches!(
                &decl.init,
                Some(Expression::ArrowFunctionExpression(_))
                    | Some(Expression::FunctionExpression(_))
            );

            let kind = if is_arrow_or_fn { "fn" } else { "const" };
            let var_fqn = format!("{}::{}", self.rel_module, name);

            self.symbols.push(SymbolDef {
                name: name.to_string(),
                kind: kind.to_string(),
                file: self.file_path_str.clone(),
                line: start_line,
                language: "typescript".to_string(),
                end_line,
                qualified_name: var_fqn.clone(),
            });

            if let Some(ref init) = decl.init {
                let inner_caller = if is_arrow_or_fn { &var_fqn } else { caller };
                self.walk_expression(init, inner_caller);
            }
        }
    }

    fn walk_statement(&mut self, stmt: &Statement<'_>, caller: &str) {
        match stmt {
            Statement::BlockStatement(b) => {
                for s in &b.body {
                    self.walk_statement(s, caller);
                }
            }
            Statement::ExpressionStatement(e) => {
                self.walk_expression(&e.expression, caller);
            }
            Statement::IfStatement(s) => {
                self.walk_expression(&s.test, caller);
                self.walk_statement(&s.consequent, caller);
                if let Some(ref alt) = s.alternate {
                    self.walk_statement(alt, caller);
                }
            }
            Statement::WhileStatement(s) => {
                self.walk_expression(&s.test, caller);
                self.walk_statement(&s.body, caller);
            }
            Statement::DoWhileStatement(s) => {
                self.walk_statement(&s.body, caller);
                self.walk_expression(&s.test, caller);
            }
            Statement::ForStatement(s) => {
                self.walk_for_statement(s, caller);
            }
            Statement::ForInStatement(s) => {
                self.walk_expression(&s.right, caller);
                self.walk_statement(&s.body, caller);
            }
            Statement::ForOfStatement(s) => {
                self.walk_expression(&s.right, caller);
                self.walk_statement(&s.body, caller);
            }
            Statement::SwitchStatement(s) => {
                self.walk_expression(&s.discriminant, caller);
                for case in &s.cases {
                    if let Some(ref test) = case.test {
                        self.walk_expression(test, caller);
                    }
                    for c_stmt in &case.consequent {
                        self.walk_statement(c_stmt, caller);
                    }
                }
            }
            Statement::ReturnStatement(s) => {
                if let Some(ref arg) = s.argument {
                    self.walk_expression(arg, caller);
                }
            }
            Statement::ThrowStatement(s) => {
                self.walk_expression(&s.argument, caller);
            }
            Statement::TryStatement(s) => {
                for st in &s.block.body {
                    self.walk_statement(st, caller);
                }
                if let Some(ref h) = s.handler {
                    for st in &h.body.body {
                        self.walk_statement(st, caller);
                    }
                }
                if let Some(ref f) = s.finalizer {
                    for st in &f.body {
                        self.walk_statement(st, caller);
                    }
                }
            }
            Statement::LabeledStatement(s) => {
                self.walk_statement(&s.body, caller);
            }
            Statement::VariableDeclaration(var) => {
                self.walk_variable_declaration(var, caller);
            }
            Statement::FunctionDeclaration(func) => {
                self.walk_function(func);
            }
            Statement::ClassDeclaration(cls) => {
                self.walk_class(cls);
            }
            _ => {}
        }
    }

    fn walk_for_statement(&mut self, s: &ForStatement<'_>, caller: &str) {
        if let Some(ref init) = s.init {
            match init {
                ForStatementInit::VariableDeclaration(v) => {
                    self.walk_variable_declaration(v, caller);
                }
                _ => {
                    if let Some(e) = init.as_expression() {
                        self.walk_expression(e, caller);
                    }
                }
            }
        }
        if let Some(ref test) = s.test {
            self.walk_expression(test, caller);
        }
        if let Some(ref update) = s.update {
            self.walk_expression(update, caller);
        }
        self.walk_statement(&s.body, caller);
    }

    fn walk_expression(&mut self, expr: &Expression<'_>, caller: &str) {
        match expr {
            Expression::CallExpression(call) => {
                self.handle_call_expression(call, caller);
            }
            Expression::NewExpression(new_expr) => {
                self.handle_new_expression(new_expr, caller);
            }
            Expression::ArrowFunctionExpression(arrow) => {
                for stmt in &arrow.body.statements {
                    self.walk_statement(stmt, caller);
                }
            }
            Expression::FunctionExpression(func) => {
                if let Some(ref body) = func.body {
                    for stmt in &body.statements {
                        self.walk_statement(stmt, caller);
                    }
                }
            }
            Expression::BinaryExpression(b) => {
                self.walk_expression(&b.left, caller);
                self.walk_expression(&b.right, caller);
            }
            Expression::LogicalExpression(l) => {
                self.walk_expression(&l.left, caller);
                self.walk_expression(&l.right, caller);
            }
            Expression::UnaryExpression(u) => {
                self.walk_expression(&u.argument, caller);
            }
            Expression::AssignmentExpression(a) => {
                self.walk_expression(&a.right, caller);
            }
            Expression::ConditionalExpression(c) => {
                self.walk_expression(&c.test, caller);
                self.walk_expression(&c.consequent, caller);
                self.walk_expression(&c.alternate, caller);
            }
            Expression::ParenthesizedExpression(p) => {
                self.walk_expression(&p.expression, caller);
            }
            Expression::SequenceExpression(s) => {
                for e in &s.expressions {
                    self.walk_expression(e, caller);
                }
            }
            Expression::ArrayExpression(arr) => {
                for el in &arr.elements {
                    if let Some(e) = el.as_expression() {
                        self.walk_expression(e, caller);
                    } else if let ArrayExpressionElement::SpreadElement(s) = el {
                        self.walk_expression(&s.argument, caller);
                    }
                }
            }
            Expression::ObjectExpression(obj) => {
                for prop in &obj.properties {
                    match prop {
                        ObjectPropertyKind::ObjectProperty(p) => {
                            self.walk_expression(&p.value, caller);
                        }
                        ObjectPropertyKind::SpreadProperty(s) => {
                            self.walk_expression(&s.argument, caller);
                        }
                    }
                }
            }
            Expression::AwaitExpression(a) => {
                self.walk_expression(&a.argument, caller);
            }
            Expression::YieldExpression(y) => {
                if let Some(ref arg) = y.argument {
                    self.walk_expression(arg, caller);
                }
            }
            Expression::ChainExpression(c) => {
                self.walk_chain_expression(c, caller);
            }
            Expression::StaticMemberExpression(m) => {
                self.walk_expression(&m.object, caller);
            }
            Expression::ComputedMemberExpression(m) => {
                self.walk_expression(&m.object, caller);
                self.walk_expression(&m.expression, caller);
            }
            Expression::TemplateLiteral(t) => {
                for e in &t.expressions {
                    self.walk_expression(e, caller);
                }
            }
            Expression::TaggedTemplateExpression(t) => {
                self.walk_expression(&t.tag, caller);
                for e in &t.quasi.expressions {
                    self.walk_expression(e, caller);
                }
            }
            Expression::TSAsExpression(a) => {
                self.walk_expression(&a.expression, caller);
            }
            Expression::TSTypeAssertion(a) => {
                self.walk_expression(&a.expression, caller);
            }
            Expression::TSNonNullExpression(n) => {
                self.walk_expression(&n.expression, caller);
            }
            Expression::TSInstantiationExpression(i) => {
                self.walk_expression(&i.expression, caller);
            }
            Expression::JSXElement(jsx) => {
                self.walk_jsx_element(jsx, caller);
            }
            Expression::JSXFragment(frag) => {
                self.walk_jsx_children(&frag.children, caller);
            }
            _ => {}
        }
    }

    fn handle_call_expression(&mut self, call: &CallExpression<'_>, caller: &str) {
        let line = self.line_index.line_number(call.span.start as usize) as u32;
        let target = self.resolve_callee_target(&call.callee);
        self.calls.push(RawCallSite {
            caller_fqn: caller.to_string(),
            call_line: line,
            target,
        });
        self.walk_expression(&call.callee, caller);
        for arg in &call.arguments {
            if let Some(e) = arg.as_expression() {
                self.walk_expression(e, caller);
            } else if let Argument::SpreadElement(s) = arg {
                self.walk_expression(&s.argument, caller);
            }
        }
    }

    fn handle_new_expression(&mut self, new_expr: &NewExpression<'_>, caller: &str) {
        let line = self.line_index.line_number(new_expr.span.start as usize) as u32;
        let target = self.resolve_callee_target(&new_expr.callee);
        self.calls.push(RawCallSite {
            caller_fqn: caller.to_string(),
            call_line: line,
            target,
        });
        self.walk_expression(&new_expr.callee, caller);
        for arg in &new_expr.arguments {
            if let Some(e) = arg.as_expression() {
                self.walk_expression(e, caller);
            } else if let Argument::SpreadElement(s) = arg {
                self.walk_expression(&s.argument, caller);
            }
        }
    }

    fn walk_chain_expression(&mut self, c: &ChainExpression<'_>, caller: &str) {
        match &c.expression {
            ChainElement::CallExpression(call) => {
                let line = self.line_index.line_number(call.span.start as usize) as u32;
                let target = self.resolve_callee_target(&call.callee);
                self.calls.push(RawCallSite {
                    caller_fqn: caller.to_string(),
                    call_line: line,
                    target,
                });
                self.walk_expression(&call.callee, caller);
                for arg in &call.arguments {
                    if let Some(e) = arg.as_expression() {
                        self.walk_expression(e, caller);
                    } else if let Argument::SpreadElement(s) = arg {
                        self.walk_expression(&s.argument, caller);
                    }
                }
            }
            ChainElement::StaticMemberExpression(m) => {
                self.walk_expression(&m.object, caller);
            }
            ChainElement::ComputedMemberExpression(m) => {
                self.walk_expression(&m.object, caller);
                self.walk_expression(&m.expression, caller);
            }
            _ => {}
        }
    }

    fn walk_jsx_element(&mut self, jsx: &JSXElement<'_>, caller: &str) {
        for attr in &jsx.opening_element.attributes {
            if let JSXAttributeItem::Attribute(a) = attr {
                if let Some(JSXAttributeValue::ExpressionContainer(c)) = &a.value {
                    if let Some(e) = c.expression.as_expression() {
                        self.walk_expression(e, caller);
                    }
                }
            }
        }
        self.walk_jsx_children(&jsx.children, caller);
    }

    fn walk_jsx_children(&mut self, children: &[JSXChild<'_>], caller: &str) {
        for child in children {
            match child {
                JSXChild::Element(e) => self.walk_jsx_element(e, caller),
                JSXChild::ExpressionContainer(c) => {
                    if let Some(e) = c.expression.as_expression() {
                        self.walk_expression(e, caller);
                    }
                }
                JSXChild::Fragment(f) => self.walk_jsx_children(&f.children, caller),
                _ => {}
            }
        }
    }

    fn resolve_callee_target(&self, callee: &Expression<'_>) -> RawCalleeTarget {
        match callee {
            Expression::Identifier(ident) => {
                let name = ident.name.as_str();
                if let Some(binding) = self.imports.get(name) {
                    RawCalleeTarget::Path(format!("{}::{}", binding.specifier, binding.imported_name))
                } else if self.local_declarations.contains(name) {
                    RawCalleeTarget::Path(format!("{}::{}", self.rel_module, name))
                } else {
                    RawCalleeTarget::Path(name.to_string())
                }
            }
            Expression::StaticMemberExpression(expr) => {
                self.resolve_static_member_target(expr)
            }
            Expression::ParenthesizedExpression(p) => self.resolve_callee_target(&p.expression),
            _ => RawCalleeTarget::Path("".to_string()),
        }
    }

    fn resolve_static_member_target(&self, expr: &StaticMemberExpression<'_>) -> RawCalleeTarget {
        let prop = expr.property.name.as_str();
        match &expr.object {
            Expression::Identifier(obj_id) => {
                let obj_name = obj_id.name.as_str();
                if let Some(binding) = self.imports.get(obj_name) {
                    if binding.imported_name == "*" {
                        return RawCalleeTarget::Path(format!("{}::{}", binding.specifier, prop));
                    }
                }
                if obj_name == "this" {
                    let type_name = self.current_class.clone().unwrap_or_default();
                    RawCalleeTarget::Method {
                        receiver: TypeRef::Known {
                            crate_or_pkg: "".to_string(),
                            type_name,
                        },
                        method_name: prop.to_string(),
                    }
                } else {
                    RawCalleeTarget::Method {
                        receiver: TypeRef::Known {
                            crate_or_pkg: "".to_string(),
                            type_name: obj_name.to_string(),
                        },
                        method_name: prop.to_string(),
                    }
                }
            }
            Expression::ThisExpression(_) => {
                let type_name = self.current_class.clone().unwrap_or_default();
                RawCalleeTarget::Method {
                    receiver: TypeRef::Known {
                        crate_or_pkg: "".to_string(),
                        type_name,
                    },
                    method_name: prop.to_string(),
                }
            }
            _ => {
                if let Some((root_receiver, fields)) = flatten_member_chain(&expr.object) {
                    let final_receiver = if matches!(&root_receiver, TypeRef::Known { type_name, .. } if type_name == "this") {
                        TypeRef::Known {
                            crate_or_pkg: "".to_string(),
                            type_name: self.current_class.clone().unwrap_or_default(),
                        }
                    } else {
                        root_receiver
                    };
                    RawCalleeTarget::FieldChain {
                        receiver: final_receiver,
                        fields,
                        method_name: prop.to_string(),
                    }
                } else {
                    RawCalleeTarget::Method {
                        receiver: TypeRef::Unknown,
                        method_name: prop.to_string(),
                    }
                }
            }
        }
    }

    fn extract_routes(&mut self) {
        for cap in CLIENT_ROUTE_REGEX.captures_iter(self.content) {
            if let Some(m) = cap.get(0) {
                let route = cap.get(1).map(|c| c.as_str()).unwrap_or("");
                let line = self.line_index.line_number(m.start());
                let caller = self.find_enclosing_caller_name(line);
                self.routes.push(RouteBinding::new(
                    route.to_string(),
                    caller,
                    self.file_path_str.clone(),
                    line,
                    false,
                ));
            }
        }
    }

    fn find_enclosing_caller_name(&self, line: usize) -> String {
        self.symbols
            .iter()
            .filter(|s| {
                (s.kind == "fn" || s.kind == "class") && s.line <= line && line <= s.end_line
            })
            .min_by_key(|s| s.end_line - s.line)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "<module>".to_string())
    }

    fn record_interface(&mut self, decl: &TSInterfaceDeclaration<'_>) {
        let name = decl.id.name.as_str();
        let start_line = self.line_index.line_number(decl.span.start as usize);
        let end_line = self
            .line_index
            .line_number(decl.span.end.saturating_sub(1) as usize)
            .max(start_line);
        self.symbols.push(SymbolDef {
            name: name.to_string(),
            kind: "interface".to_string(),
            file: self.file_path_str.clone(),
            line: start_line,
            language: "typescript".to_string(),
            end_line,
            qualified_name: format!("{}::{}", self.rel_module, name),
        });
    }

    fn record_type_alias(&mut self, decl: &TSTypeAliasDeclaration<'_>) {
        let name = decl.id.name.as_str();
        let start_line = self.line_index.line_number(decl.span.start as usize);
        let end_line = self
            .line_index
            .line_number(decl.span.end.saturating_sub(1) as usize)
            .max(start_line);
        self.symbols.push(SymbolDef {
            name: name.to_string(),
            kind: "type".to_string(),
            file: self.file_path_str.clone(),
            line: start_line,
            language: "typescript".to_string(),
            end_line,
            qualified_name: format!("{}::{}", self.rel_module, name),
        });
    }

    fn record_enum(&mut self, decl: &TSEnumDeclaration<'_>) {
        let name = decl.id.name.as_str();
        let start_line = self.line_index.line_number(decl.span.start as usize);
        let end_line = self
            .line_index
            .line_number(decl.span.end.saturating_sub(1) as usize)
            .max(start_line);
        self.symbols.push(SymbolDef {
            name: name.to_string(),
            kind: "enum".to_string(),
            file: self.file_path_str.clone(),
            line: start_line,
            language: "typescript".to_string(),
            end_line,
            qualified_name: format!("{}::{}", self.rel_module, name),
        });
    }
}

fn collect_declaration_names(stmt: &Statement<'_>, names: &mut HashSet<String>) {
    match stmt {
        Statement::FunctionDeclaration(func) => {
            if let Some(ref id) = func.id {
                names.insert(id.name.to_string());
            }
        }
        Statement::ClassDeclaration(cls) => {
            if let Some(ref id) = cls.id {
                names.insert(id.name.to_string());
            }
        }
        Statement::VariableDeclaration(var) => {
            for d in &var.declarations {
                if let Some(name) = d.id.get_identifier_name() {
                    names.insert(name.to_string());
                }
            }
        }
        Statement::ExportNamedDeclaration(decl) => {
            if let Some(ref d) = decl.declaration {
                match d {
                    Declaration::FunctionDeclaration(f) => {
                        if let Some(ref id) = f.id {
                            names.insert(id.name.to_string());
                        }
                    }
                    Declaration::ClassDeclaration(c) => {
                        if let Some(ref id) = c.id {
                            names.insert(id.name.to_string());
                        }
                    }
                    Declaration::VariableDeclaration(v) => {
                        for d in &v.declarations {
                            if let Some(name) = d.id.get_identifier_name() {
                                names.insert(name.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn extract_type_name_from_annotation(type_ann: &TSTypeAnnotation<'_>) -> Option<String> {
    if let TSType::TSTypeReference(t_ref) = &type_ann.type_annotation {
        if let TSTypeName::IdentifierReference(id) = &t_ref.type_name {
            return Some(id.name.to_string());
        }
    }
    None
}

fn flatten_member_chain(expr: &Expression<'_>) -> Option<(TypeRef, Vec<String>)> {
    let mut fields = Vec::new();
    let mut current = expr;
    loop {
        match current {
            Expression::StaticMemberExpression(m) => {
                fields.push(m.property.name.to_string());
                current = &m.object;
            }
            Expression::Identifier(id) => {
                fields.reverse();
                let type_ref = TypeRef::Known {
                    crate_or_pkg: "".to_string(),
                    type_name: id.name.to_string(),
                };
                return Some((type_ref, fields));
            }
            Expression::ThisExpression(_) => {
                fields.reverse();
                let type_ref = TypeRef::Known {
                    crate_or_pkg: "".to_string(),
                    type_name: "this".to_string(),
                };
                return Some((type_ref, fields));
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_ts_symbols_and_spans() {
        let code = r#"
import { Helper } from './helper';

export interface UserPayload {
    id: string;
    name: string;
}

export type UserId = string;

export enum Status {
    Active,
    Inactive,
}

export class UserService {
    private client: Helper;

    constructor() {
        this.client = new Helper();
    }

    public async login(userId: UserId): Promise<UserPayload> {
        return this.client.execute(userId);
    }
}

export async function fetchUser(id: string): Promise<UserPayload> {
    const service = new UserService();
    return service.login(id);
}

export const helperArrow = (x: number) => {
    return x * 2;
};
"#;

        let opts = TsFactOptions {
            file_path: Path::new("/workspace/frontend/src/user.ts"),
            rel_path: Path::new("frontend/src/user.ts"),
            package_name: Some("@acme/frontend"),
            omit_tests: false,
        };

        let facts = extract_ts_file_facts(&opts, code).expect("extract facts");
        assert_eq!(facts.crate_or_pkg, "@acme/frontend");

        let sym_fqns: Vec<_> = facts.symbols.iter().map(|s| s.qualified_name.as_str()).collect();
        assert!(sym_fqns.contains(&"frontend/src/user::UserPayload"));
        assert!(sym_fqns.contains(&"frontend/src/user::UserId"));
        assert!(sym_fqns.contains(&"frontend/src/user::Status"));
        assert!(sym_fqns.contains(&"frontend/src/user::UserService"));
        assert!(sym_fqns.contains(&"frontend/src/user::UserService.login"));
        assert!(sym_fqns.contains(&"frontend/src/user::fetchUser"));
        assert!(sym_fqns.contains(&"frontend/src/user::helperArrow"));

        for sym in &facts.symbols {
            assert!(sym.line > 0);
            assert!(sym.end_line >= sym.line);
        }

        assert_eq!(facts.exports.len(), 6);
        assert!(facts.exports.contains(&ExportEntry::Local("UserPayload".to_string())));
        assert!(facts.exports.contains(&ExportEntry::Local("UserService".to_string())));
        assert!(facts.exports.contains(&ExportEntry::Local("fetchUser".to_string())));
    }

    #[test]
    fn test_extract_ts_barrel_and_imports() {
        let code = r#"
export { Button, Input as TextInput } from './components';
export * from './theme';
export default function main() {}
"#;

        let opts = TsFactOptions {
            file_path: Path::new("/workspace/src/index.ts"),
            rel_path: Path::new("src/index.ts"),
            package_name: None,
            omit_tests: false,
        };

        let facts = extract_ts_file_facts(&opts, code).expect("extract facts");
        assert_eq!(facts.crate_or_pkg, "src/index");

        assert!(facts.exports.contains(&ExportEntry::NamedReExport {
            specifier: "./components".to_string(),
            imported: "Button".to_string(),
            exported: "Button".to_string(),
        }));
        assert!(facts.exports.contains(&ExportEntry::NamedReExport {
            specifier: "./components".to_string(),
            imported: "Input".to_string(),
            exported: "TextInput".to_string(),
        }));
        assert!(facts.exports.contains(&ExportEntry::StarReExport {
            specifier: "./theme".to_string(),
        }));
        assert!(facts.exports.contains(&ExportEntry::Local("default".to_string())));
    }

    #[test]
    fn test_extract_ts_calls_and_enclosing_scopes() {
        let code = r#"
import { query } from './db';
import * as api from './api';

export class Service {
    run() {
        query();
        api.sendMetrics();
        this.helper();
    }

    helper() {
        const h = new Handler();
    }
}

function topFn() {
    const s = new Service();
    s.run();
}
"#;

        let opts = TsFactOptions {
            file_path: Path::new("/workspace/src/service.ts"),
            rel_path: Path::new("src/service.ts"),
            package_name: None,
            omit_tests: false,
        };

        let facts = extract_ts_file_facts(&opts, code).expect("extract facts");

        let run_calls: Vec<_> = facts
            .calls
            .iter()
            .filter(|c| c.caller_fqn == "src/service::Service.run")
            .collect();
        assert_eq!(run_calls.len(), 3);

        let query_call = run_calls
            .iter()
            .find(|c| matches!(&c.target, RawCalleeTarget::Path(p) if p == "./db::query"));
        assert!(query_call.is_some(), "should resolve imported query()");
        assert!(query_call.unwrap().call_line > 0);

        let ns_call = run_calls
            .iter()
            .find(|c| matches!(&c.target, RawCalleeTarget::Path(p) if p == "./api::sendMetrics"));
        assert!(ns_call.is_some(), "should resolve namespace api.sendMetrics()");

        let helper_call = run_calls
            .iter()
            .find(|c| matches!(&c.target, RawCalleeTarget::Method { method_name, .. } if method_name == "helper"));
        assert!(helper_call.is_some(), "should resolve this.helper()");
    }

    #[test]
    fn test_extract_ts_routes() {
        let code = r#"
export async function loadUsers() {
    const res = await fetch("/api/v1/users");
    return res.json();
}

export const createUser = async (data: any) => {
    return axios.post('/api/v1/users/create', data);
};
"#;

        let opts = TsFactOptions {
            file_path: Path::new("/workspace/src/apiClient.ts"),
            rel_path: Path::new("src/apiClient.ts"),
            package_name: None,
            omit_tests: false,
        };

        let facts = extract_ts_file_facts(&opts, code).expect("extract facts");
        assert_eq!(facts.routes.len(), 2);

        let fetch_route = facts
            .routes
            .iter()
            .find(|r| r.route_path == "/api/v1/users")
            .expect("fetch route");
        assert_eq!(fetch_route.handler_or_caller, "loadUsers");
        assert!(!fetch_route.is_backend);
        assert!(fetch_route.line > 0);

        let post_route = facts
            .routes
            .iter()
            .find(|r| r.route_path == "/api/v1/users/create")
            .expect("post route");
        assert_eq!(post_route.handler_or_caller, "createUser");
        assert!(!post_route.is_backend);
    }
}
