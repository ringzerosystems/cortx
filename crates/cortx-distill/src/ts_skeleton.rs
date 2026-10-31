//! TypeScript and TSX AST interface skeleton stripper using oxc_parser.

use crate::error::DistillError;
use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};
use std::path::Path;

/// Strips implementation logic, JSX, and local hooks from TypeScript / TSX source,
/// preserving exported functions, classes, interfaces, type aliases, and enums.
pub fn extract_ts_skeleton(source_code: &str, is_jsx: bool) -> Result<String, DistillError> {
    extract_ts_skeleton_with_options(source_code, is_jsx, true)
}

/// Strips implementation logic, JSX, and local hooks from TypeScript / TSX source with
/// configurable options for private types and constants.
pub fn extract_ts_skeleton_with_options(
    source_code: &str,
    is_jsx: bool,
    include_private: bool,
) -> Result<String, DistillError> {
    let allocator = Allocator::default();
    let mut source_type = SourceType::default().with_typescript(true);
    if is_jsx {
        source_type = source_type.with_jsx(true);
    }

    let ret = Parser::new(&allocator, source_code, source_type).parse();
    if !ret.errors.is_empty() && ret.program.body.is_empty() {
        return Err(DistillError::SymbolNotFound(
            "Failed to parse TypeScript/TSX syntax tree".to_string(),
        ));
    }

    let mut output = String::with_capacity(source_code.len() / 4);
    let mut items = Vec::new();

    for stmt in &ret.program.body {
        if let Some(item_str) = format_statement(stmt, source_code, include_private) {
            let trimmed = item_str.trim();
            if !trimmed.is_empty() {
                let final_str = if !matches!(stmt, Statement::ImportDeclaration(_)) {
                    if let Some(jsdoc) =
                        find_leading_jsdoc(&ret.program.comments, stmt.span().start, source_code)
                    {
                        format!("{}\n{}", jsdoc.trim(), trimmed)
                    } else {
                        trimmed.to_string()
                    }
                } else {
                    trimmed.to_string()
                };
                items.push(final_str);
            }
        }
    }

    output.push_str(&items.join("\n\n"));
    Ok(output)
}

#[inline]
fn safe_slice(src: &str, start: usize, end: usize) -> Option<&str> {
    if start <= end && end <= src.len() && src.is_char_boundary(start) && src.is_char_boundary(end)
    {
        Some(&src[start..end])
    } else {
        None
    }
}

fn extract_parenthesized_inner(src: &str, span_start: usize, span_end: usize) -> &str {
    if let Some(slice) = safe_slice(src, span_start, span_end) {
        let trimmed = slice.trim();
        if trimmed.starts_with('(') && trimmed.ends_with(')') && trimmed.len() >= 2 {
            return trimmed[1..trimmed.len() - 1].trim();
        }
        return trimmed;
    }
    ""
}

/// Finds leading JSDoc (`/** ... */`) immediately preceding `stmt_start`.
fn find_leading_jsdoc<'a>(comments: &[Comment], stmt_start: u32, src: &'a str) -> Option<&'a str> {
    let mut idx = comments.partition_point(|c| c.span.end <= stmt_start);
    let mut last_end = stmt_start as usize;

    while idx > 0 {
        idx -= 1;
        let comment = &comments[idx];
        let end = comment.span.end as usize;
        let start = comment.span.start as usize;

        let between = safe_slice(src, end, last_end)?;
        if !between.trim().is_empty() {
            return None;
        }

        let comment_text = safe_slice(src, start, end)?;
        if comment_text.starts_with("/**")
            && comment_text.ends_with("*/")
            && !comment_text.starts_with("/***")
        {
            return Some(comment_text);
        }

        last_end = start;
    }

    None
}

/// Reads a TypeScript or TSX file from disk and extracts its type skeleton.
pub fn extract_ts_file_skeleton(path: &Path) -> Result<String, DistillError> {
    extract_ts_file_skeleton_with_options(path, true)
}

/// Reads a TypeScript or TSX file from disk and extracts its type skeleton with options.
pub fn extract_ts_file_skeleton_with_options(
    path: &Path,
    include_private: bool,
) -> Result<String, DistillError> {
    let content = std::fs::read_to_string(path).map_err(DistillError::Io)?;
    let is_jsx = path
        .extension()
        .is_some_and(|ext| ext == "tsx" || ext == "jsx");
    extract_ts_skeleton_with_options(&content, is_jsx, include_private)
}

fn format_statement(stmt: &Statement<'_>, src: &str, include_private: bool) -> Option<String> {
    match stmt {
        Statement::ImportDeclaration(decl) => {
            let has_no_specifiers = decl
                .specifiers
                .as_ref()
                .is_none_or(|specs| specs.is_empty());
            let src_val = decl.source.value.as_str();
            let clean_path = src_val.split('?').next().unwrap_or(src_val);
            let is_stylesheet = clean_path.ends_with(".css")
                || clean_path.ends_with(".scss")
                || clean_path.ends_with(".sass")
                || clean_path.ends_with(".less");
            if has_no_specifiers && is_stylesheet {
                None
            } else {
                let span = decl.span;
                safe_slice(src, span.start as usize, span.end as usize).map(|s| s.to_string())
            }
        }
        Statement::ExportAllDeclaration(decl) => {
            let span = decl.span;
            safe_slice(src, span.start as usize, span.end as usize).map(|s| s.to_string())
        }
        Statement::ExportNamedDeclaration(decl) => {
            if let Some(ref d) = decl.declaration {
                format_declaration(d, src, true, include_private)
            } else {
                let span = decl.span;
                safe_slice(src, span.start as usize, span.end as usize).map(|s| s.to_string())
            }
        }
        Statement::ExportDefaultDeclaration(decl) => match &decl.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(func) => {
                format_function(func, src, true, true)
            }
            ExportDefaultDeclarationKind::ClassDeclaration(cls) => {
                format_class(cls, src, true, true)
            }
            _ => {
                let span = decl.span;
                safe_slice(src, span.start as usize, span.end as usize).map(|s| s.to_string())
            }
        },
        Statement::TSInterfaceDeclaration(decl) => {
            if !include_private {
                None
            } else {
                let span = decl.span;
                safe_slice(src, span.start as usize, span.end as usize).map(|s| s.to_string())
            }
        }
        Statement::TSTypeAliasDeclaration(decl) => {
            if !include_private {
                None
            } else {
                let span = decl.span;
                safe_slice(src, span.start as usize, span.end as usize).map(|s| s.to_string())
            }
        }
        Statement::TSEnumDeclaration(decl) => {
            if !include_private {
                None
            } else {
                let span = decl.span;
                safe_slice(src, span.start as usize, span.end as usize).map(|s| s.to_string())
            }
        }
        Statement::FunctionDeclaration(func) => {
            if !include_private {
                None
            } else {
                format_function(func, src, false, false)
            }
        }
        Statement::ClassDeclaration(cls) => {
            if !include_private {
                None
            } else {
                format_class(cls, src, false, false)
            }
        }
        Statement::VariableDeclaration(var) => {
            format_variable_declaration(var, src, false, include_private)
        }
        _ => None,
    }
}

fn format_declaration(
    decl: &Declaration<'_>,
    src: &str,
    is_export: bool,
    include_private: bool,
) -> Option<String> {
    match decl {
        Declaration::TSInterfaceDeclaration(d) => {
            let span = d.span;
            let text = safe_slice(src, span.start as usize, span.end as usize)?;
            if is_export && !text.starts_with("export") {
                Some(format!("export {}", text))
            } else {
                Some(text.to_string())
            }
        }
        Declaration::TSTypeAliasDeclaration(d) => {
            let span = d.span;
            let text = safe_slice(src, span.start as usize, span.end as usize)?;
            if is_export && !text.starts_with("export") {
                Some(format!("export {}", text))
            } else {
                Some(text.to_string())
            }
        }
        Declaration::TSEnumDeclaration(d) => {
            let span = d.span;
            let text = safe_slice(src, span.start as usize, span.end as usize)?;
            if is_export && !text.starts_with("export") {
                Some(format!("export {}", text))
            } else {
                Some(text.to_string())
            }
        }
        Declaration::FunctionDeclaration(func) => format_function(func, src, is_export, false),
        Declaration::ClassDeclaration(cls) => format_class(cls, src, is_export, false),
        Declaration::VariableDeclaration(var) => {
            format_variable_declaration(var, src, is_export, include_private)
        }
        _ => None,
    }
}

fn is_simple_literal(expr: &Expression<'_>) -> bool {
    match expr {
        Expression::NumericLiteral(_)
        | Expression::StringLiteral(_)
        | Expression::BooleanLiteral(_)
        | Expression::BigIntLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::RegExpLiteral(_) => true,
        Expression::UnaryExpression(unary) => matches!(
            unary.argument,
            Expression::NumericLiteral(_) | Expression::BigIntLiteral(_)
        ),
        Expression::TemplateLiteral(t) => t.expressions.is_empty(),
        Expression::ParenthesizedExpression(p) => is_simple_literal(&p.expression),
        _ => false,
    }
}

fn format_variable_declaration(
    var: &VariableDeclaration<'_>,
    src: &str,
    is_export: bool,
    include_private: bool,
) -> Option<String> {
    if !is_export && !include_private {
        return None;
    }
    if var.declarations.is_empty() {
        return None;
    }

    let kind_str = var.kind.as_str();
    let prefix = match (is_export, var.declare) {
        (true, true) => format!("export declare {kind_str} "),
        (true, false) => format!("export {kind_str} "),
        (false, true) => format!("declare {kind_str} "),
        (false, false) => format!("{kind_str} "),
    };

    let mut parts = Vec::with_capacity(var.declarations.len());
    for d in &var.declarations {
        let id_start = d.id.span().start as usize;
        if let Some(ref type_ann) = d.id.type_annotation {
            let id_end = type_ann.span.end as usize;
            if let Some(id_s) = safe_slice(src, id_start, id_end) {
                parts.push(id_s.trim().to_string());
            }
        } else if let Some(ref init_expr) = d.init {
            let id_end = d.id.span().end as usize;
            if let Some(id_str) = safe_slice(src, id_start, id_end).map(|s| s.trim()) {
                if is_simple_literal(init_expr) {
                    let init_span = init_expr.span();
                    let init_start = init_span.start as usize;
                    let init_end = init_span.end as usize;
                    if let Some(init_str) = safe_slice(src, init_start, init_end).map(|s| s.trim())
                    {
                        parts.push(format!("{id_str} = {init_str}"));
                    } else {
                        parts.push(id_str.to_string());
                    }
                } else {
                    parts.push(id_str.to_string());
                }
            }
        } else {
            let id_end = d.id.span().end as usize;
            if let Some(id_s) = safe_slice(src, id_start, id_end) {
                parts.push(id_s.trim().to_string());
            }
        }
    }

    if parts.is_empty() {
        return None;
    }

    Some(format!("{}{};", prefix, parts.join(", ")))
}

fn format_function(
    func: &Function<'_>,
    src: &str,
    is_export: bool,
    is_default: bool,
) -> Option<String> {
    let name = func.id.as_ref().map(|id| id.name.as_str()).unwrap_or("");
    let is_component =
        name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) || has_jsx_return(func);

    let prefix = match (is_export, is_default) {
        (true, true) => "export default ",
        (true, false) => "export ",
        (false, true) => "default ",
        (false, false) => "",
    };

    let async_str = if func.r#async { "async " } else { "" };

    let type_params = func
        .type_parameters
        .as_ref()
        .and_then(|tp| safe_slice(src, tp.span.start as usize, tp.span.end as usize))
        .unwrap_or("");

    // Format params
    let params_str = if is_component && func.params.items.len() == 1 {
        let first_param = &func.params.items[0];
        if let Some(type_ann) = &first_param.pattern.type_annotation {
            let span = type_ann.type_annotation.span();
            let type_name =
                safe_slice(src, span.start as usize, span.end as usize).unwrap_or("any");
            format!("props: {}", type_name)
        } else {
            let span = func.params.span;
            extract_parenthesized_inner(src, span.start as usize, span.end as usize).to_string()
        }
    } else {
        let span = func.params.span;
        extract_parenthesized_inner(src, span.start as usize, span.end as usize).to_string()
    };

    // Return type
    let return_str = if let Some(ref ret_type) = func.return_type {
        let span = ret_type.type_annotation.span();
        if let Some(ret_s) = safe_slice(src, span.start as usize, span.end as usize) {
            format!(": {}", ret_s)
        } else {
            String::new()
        }
    } else if is_component {
        ": JSX.Element".to_string()
    } else {
        String::new()
    };

    Some(format!(
        "{prefix}{async_str}function {name}{type_params}({params_str}){return_str};"
    ))
}

fn has_jsx_return(func: &Function<'_>) -> bool {
    if let Some(body) = &func.body {
        for stmt in &body.statements {
            if let Statement::ReturnStatement(ret) = stmt {
                if let Some(ref arg) = ret.argument {
                    if matches!(arg, Expression::JSXElement(_) | Expression::JSXFragment(_)) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn format_class(cls: &Class<'_>, src: &str, is_export: bool, is_default: bool) -> Option<String> {
    let name = cls.id.as_ref().map(|id| id.name.as_str()).unwrap_or("");
    let prefix = match (is_export, is_default) {
        (true, true) => "export default ",
        (true, false) => "export ",
        (false, true) => "default ",
        (false, false) => "",
    };

    let mut out = format!("{}class {} {{\n", prefix, name);
    for element in &cls.body.body {
        match element {
            ClassElement::MethodDefinition(method) => {
                let vis_str = method
                    .accessibility
                    .map(|a| format!("{} ", a.as_str()))
                    .unwrap_or_default();
                let static_str = if method.r#static { "static " } else { "" };
                let override_str = if method.r#override { "override " } else { "" };
                let async_str = if method.value.r#async { "async " } else { "" };
                let m_name = method
                    .key
                    .name()
                    .unwrap_or(std::borrow::Cow::Borrowed("method"));
                let type_params = method
                    .value
                    .type_parameters
                    .as_ref()
                    .and_then(|tp| safe_slice(src, tp.span.start as usize, tp.span.end as usize))
                    .unwrap_or("");
                let span = method.value.params.span;
                let params =
                    safe_slice(src, span.start as usize, span.end as usize).unwrap_or("()");
                let ret_str = if let Some(ref r) = method.value.return_type {
                    let r_span = r.type_annotation.span();
                    if let Some(ret_s) = safe_slice(src, r_span.start as usize, r_span.end as usize)
                    {
                        format!(": {}", ret_s)
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };
                out.push_str(&format!(
                    "    {vis_str}{static_str}{override_str}{async_str}{m_name}{type_params}{params}{ret_str};\n"
                ));
            }
            ClassElement::PropertyDefinition(prop) => {
                let p_span = prop.span;
                if let Some(prop_s) = safe_slice(src, p_span.start as usize, p_span.end as usize) {
                    out.push_str(&format!("    {};\n", prop_s));
                }
            }
            _ => {}
        }
    }
    out.push('}');
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_user_dashboard_tsx() {
        let code = r#"
import React, { useState, useEffect } from "react";
import { User, fetchMetrics } from "../api";

export interface UserDashboardProps {
    userId: string;
    refreshIntervalMs?: number;
    onStatusChange?: (status: "online" | "idle" | "offline") => void;
}

export function UserDashboard({ userId, refreshIntervalMs = 5000, onStatusChange }: UserDashboardProps) {
    const [metrics, setMetrics] = useState<User | null>(null);
    useEffect(() => {
        // ... 600 lines ...
    }, [userId]);

    return (
        <div className="flex flex-col p-4 bg-gray-900 border border-gray-800 rounded-xl">
            {/* ... 200 lines of JSX DOM nodes ... */}
        </div>
    );
}
"#;

        let res = extract_ts_skeleton(code, true).unwrap();
        assert!(res.contains("import React, { useState, useEffect } from \"react\";"));
        assert!(res.contains("import { User, fetchMetrics } from \"../api\";"));
        assert!(res.contains("export interface UserDashboardProps"));
        assert!(res.contains("userId: string;"));
        assert!(
            res.contains("export function UserDashboard(props: UserDashboardProps): JSX.Element;")
        );
        assert!(!res.contains("setMetrics"));
        assert!(!res.contains("600 lines"));
        assert!(!res.contains("className"));
    }

    #[test]
    fn test_import_preservation_and_stylesheet_filtering() {
        let code = r#"
import "./global.css";
import "./theme.scss";
import "./vendor.sass";
import "./legacy.less";
import "./reset.css?raw";
import "reflect-metadata";
import type { UserRole } from "./types";
import styles from "./button.module.css";
import * as path from "node:path";
import DefaultComponent from "./DefaultComponent";

export function getRole(role: UserRole): string {
    return role;
}
"#;
        let res = extract_ts_skeleton(code, false).unwrap();
        assert!(!res.contains("global.css"));
        assert!(!res.contains("theme.scss"));
        assert!(!res.contains("vendor.sass"));
        assert!(!res.contains("legacy.less"));
        assert!(!res.contains("reset.css"));
        assert!(res.contains("import \"reflect-metadata\";"));
        assert!(res.contains("import type { UserRole } from \"./types\";"));
        assert!(res.contains("import styles from \"./button.module.css\";"));
        assert!(res.contains("import * as path from \"node:path\";"));
        assert!(res.contains("import DefaultComponent from \"./DefaultComponent\";"));
        assert!(res.contains("function getRole(role: UserRole): string;"));
    }

    #[test]
    fn test_tsx_distill_throughput_and_skeleton_size() {
        let mut source = String::new();
        source.push_str("import React, { useState } from 'react';\n\n");
        source.push_str("export interface ItemProps { id: string; name: string; }\n\n");
        for i in 0..150 {
            source.push_str(&format!(
                "export function Component{i}(props: ItemProps) {{\n    const [val, setVal] = useState({i});\n    const [loading, setLoading] = useState(false);\n    useEffect(() => {{\n        console.log(\"Mounting component {i}\");\n    }}, []);\n    return (\n        <div className=\"flex flex-col p-4 bg-gray-900 border border-gray-800 rounded-xl shadow-lg\">\n            <h1 className=\"text-xl font-bold text-white\">Component {i}</h1>\n            <p className=\"text-sm text-gray-400\">Current value: {{val}}</p>\n            <button className=\"px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700\" onClick={{() => setVal(val + 1)}}>\n                Increment\n            </button>\n        </div>\n    );\n}}\n\n"
            ));
        }

        let total_lines = source.lines().count();
        assert!(total_lines >= 600);

        let start = std::time::Instant::now();
        let skeleton = extract_ts_skeleton(&source, true).unwrap();
        let elapsed = start.elapsed();

        let raw_len = source.len();
        let skeleton_len = skeleton.len();
        let size_ratio = (raw_len - skeleton_len) as f64 / raw_len as f64 * 100.0;

        assert!(
            size_ratio > 75.0,
            "Expected > 75% skeleton size reduction, got {:.2}%",
            size_ratio
        );
        // Ensure parsing is sub-millisecond in release, and well within 15ms in debug
        assert!(
            elapsed.as_millis() < 50,
            "Parsing took too long: {:?}",
            elapsed
        );
    }

    #[test]
    fn test_declaration_jsdoc_preservation_and_inline_comment_omission() {
        let code = r#"
import type { UserRole } from "./types";

/**
 * Checks if an account is within its first 24 hours of existence.
 * @param createdAt ISO timestamp string or null
 * @param now current timestamp in milliseconds
 */
export function isWithinFirst24h(createdAt: string | null | undefined, now: number = Date.now()): boolean {
    // Ordinary line comment that must be omitted
    /* Ordinary block comment that must be omitted */
    const createdMs = Date.parse(createdAt ?? "");
    if (isNaN(createdMs)) return false;
    return now - createdMs < 86_400_000;
}

/**
 * Configuration options for user session.
 */
export interface SessionConfig {
    /** Token time-to-live in seconds. */
    ttl: number;
}

/**
 * Supported authentication provider identities.
 */
export type AuthProvider = "google" | "github" | "saml";

/**
 * Service orchestrator lifecycle states.
 */
export enum ServiceState {
    Starting,
    Running,
    Stopped,
}

/**
 * Core authentication manager.
 */
export class AuthManager {
    login(): void {
        // internal login implementation
        console.log("logging in");
    }
}

/**
 * Formats a metric value for display.
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function formatMetric(val: any): string {
    return String(val);
}

// Ordinary non-JSDoc comment above helper function
function internalHelper(): void {
    // helper body
}
"#;

        let res = extract_ts_skeleton(code, false).unwrap();

        // JSDoc blocks must be preserved
        assert!(
            res.contains("/**\n * Checks if an account is within its first 24 hours of existence.")
        );
        assert!(res.contains("export function isWithinFirst24h(createdAt: string | null | undefined, now: number = Date.now()): boolean;"));

        assert!(res.contains("/**\n * Configuration options for user session.\n */"));
        assert!(res.contains("export interface SessionConfig"));
        assert!(res.contains("/** Token time-to-live in seconds. */"));

        assert!(res.contains("/**\n * Supported authentication provider identities.\n */"));
        assert!(res.contains("export type AuthProvider = \"google\" | \"github\" | \"saml\";"));

        assert!(res.contains("/**\n * Service orchestrator lifecycle states.\n */"));
        assert!(res.contains("export enum ServiceState"));

        assert!(res.contains("/**\n * Core authentication manager.\n */"));
        assert!(res.contains("export class AuthManager"));

        // Intervening line comment skipped to preserve preceding JSDoc
        assert!(res.contains("/**\n * Formats a metric value for display.\n */"));
        assert!(res.contains("export function formatMetric(val: any): string;"));

        // Function without JSDoc preserved, but non-JSDoc comment dropped
        assert!(res.contains("function internalHelper(): void;"));
        assert!(!res.contains("Ordinary non-JSDoc comment above helper function"));

        // All internal implementation comments and logic must be omitted
        assert!(!res.contains("Ordinary line comment that must be omitted"));
        assert!(!res.contains("Ordinary block comment that must be omitted"));
        assert!(!res.contains("createdMs"));
        assert!(!res.contains("internal login implementation"));
        assert!(!res.contains("eslint-disable-next-line"));
        assert!(!res.contains("helper body"));
    }

    #[test]
    fn test_constant_declarations_and_initializer_elision() {
        let code = r#"
/**
 * Global API timeout in milliseconds.
 */
export const TIMEOUT_MS = 5000;

export const BASE_URL = "https://api.example.com";
export const IS_PRODUCTION = true;
const NEGATIVE_OFFSET = -42;

/**
 * UI problem badge variant lookup table.
 */
const PROBLEM_BADGE: Record<string, { label: string; variant: "warning" | "error" | "cyan" }> = {
    warning: { label: "Warning", variant: "warning" },
    error: { label: "Error", variant: "error" },
    cyan: { label: "Info", variant: "cyan" },
};

export const BEHAVIOR_FIELDS = {
    name: "Agent Behavior",
    nested: {
        deep: true,
        items: [1, 2, 3],
    },
};

const PRIVATE_ARRAY = ["item1", "item2", "item3"];
const PRIVATE_HANDLER = (id: string) => {
    console.log("handling", id);
};

export function dummy(): void {}
"#;

        // When include_private = true:
        let res_private = extract_ts_skeleton_with_options(code, false, true).unwrap();

        // Exported constants with simple literals preserved
        assert!(res_private.contains(
            "/**\n * Global API timeout in milliseconds.\n */\nexport const TIMEOUT_MS = 5000;"
        ));
        assert!(res_private.contains("export const BASE_URL = \"https://api.example.com\";"));
        assert!(res_private.contains("export const IS_PRODUCTION = true;"));

        // Private constant with simple literal preserved
        assert!(res_private.contains("const NEGATIVE_OFFSET = -42;"));

        // Private constant with type annotation and elided complex object body
        assert!(res_private.contains(
            "/**\n * UI problem badge variant lookup table.\n */\nconst PROBLEM_BADGE: Record<string, { label: string; variant: \"warning\" | \"error\" | \"cyan\" }>;"
        ));
        assert!(!res_private.contains("Warning"));
        assert!(!res_private.contains("Info"));

        // Exported constant with elided complex object body
        assert!(res_private.contains("export const BEHAVIOR_FIELDS;"));
        assert!(!res_private.contains("Agent Behavior"));
        assert!(!res_private.contains("deep"));

        // Private constants with complex initializers elided
        assert!(res_private.contains("const PRIVATE_ARRAY;"));
        assert!(!res_private.contains("item1"));
        assert!(res_private.contains("const PRIVATE_HANDLER;"));
        assert!(!res_private.contains("handling"));

        // When include_private = false:
        let res_public = extract_ts_skeleton_with_options(code, false, false).unwrap();
        assert!(res_public.contains("export const TIMEOUT_MS = 5000;"));
        assert!(res_public.contains("export const BEHAVIOR_FIELDS;"));
        assert!(!res_public.contains("PROBLEM_BADGE"));
        assert!(!res_public.contains("NEGATIVE_OFFSET"));
        assert!(!res_public.contains("PRIVATE_ARRAY"));
        assert!(!res_public.contains("PRIVATE_HANDLER"));
    }

    #[test]
    fn test_export_all_barrel_and_unicode_safety() {
        let code = r#"
export * from "./types";
export * as math from "./math";

export function calculateUnicode(α: string, 変数: number): string {
    return α + 変数;
}

export function noParams(): void {}
"#;
        let res = extract_ts_skeleton(code, false).unwrap();
        assert!(res.contains("export * from \"./types\";"));
        assert!(res.contains("export * as math from \"./math\";"));
        assert!(res.contains("export function calculateUnicode(α: string, 変数: number): string;"));
        assert!(res.contains("export function noParams(): void;"));
    }

    #[test]
    fn test_async_modifiers_and_semicolon_syntax() {
        let code = r#"
export async function fetchUser<T>(id: string): Promise<T> {
    const res = await fetch(`/users/${id}`);
    return await res.json();
}

async function internalCompute(val: number): Promise<number> {
    return val * 42;
}

export default async function runService(): Promise<void> {
    console.log("running");
}

export class ServiceWorker {
    public static async init(): Promise<void> {
        console.log("init");
    }

    async process(): Promise<boolean> {
        return true;
    }

    protected async cleanup(): Promise<void> {
        console.log("cleanup");
    }

    syncMethod(): void {
        console.log("sync");
    }
}
"#;

        let res = extract_ts_skeleton_with_options(code, false, true).unwrap();

        // Function declarations modifier order and semicolon termination
        assert!(res.contains("export async function fetchUser<T>(id: string): Promise<T>;"));
        assert!(res.contains("async function internalCompute(val: number): Promise<number>;"));
        assert!(res.contains("export default async function runService(): Promise<void>;"));

        // Class method modifier order and semicolon termination
        assert!(res.contains("public static async init(): Promise<void>;"));
        assert!(res.contains("async process(): Promise<boolean>;"));
        assert!(res.contains("protected async cleanup(): Promise<void>;"));
        assert!(res.contains("syncMethod(): void;"));

        // No invalid function async or trailing function bodies
        assert!(!res.contains("function async"));
        assert!(!res.contains("async export"));
    }
}
