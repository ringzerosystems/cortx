//! Go AST interface skeleton stripper using tree-sitter-go.

use crate::error::DistillError;
use crate::indexer::{ContractInfo, RouteBinding, SymbolDef};
use std::path::Path;

/// Strips implementation bodies from Go source code, preserving package,
/// imports, consts, vars, structs with tags, interfaces, and function/method signatures.
pub fn extract_go_skeleton(source_code: &str) -> Result<String, DistillError> {
    extract_go_skeleton_with_options(source_code, true, true)
}

/// Strips implementation bodies with options to include unexported functions and omit test functions.
pub fn extract_go_skeleton_with_options(
    source_code: &str,
    include_private: bool,
    omit_tests: bool,
) -> Result<String, DistillError> {
    let mut parser = tree_sitter::Parser::new();
    let lang = tree_sitter_go::LANGUAGE.into();
    parser.set_language(&lang).map_err(|e| {
        DistillError::SymbolNotFound(format!("failed to initialize tree-sitter-go: {e}"))
    })?;

    let tree = parser.parse(source_code, None).ok_or_else(|| {
        DistillError::SymbolNotFound("failed to parse Go source code".to_string())
    })?;

    let root = tree.root_node();
    let mut items = Vec::new();

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if let Some(item_str) = format_go_node(child, source_code, include_private, omit_tests) {
            let trimmed = item_str.trim();
            if !trimmed.is_empty() {
                items.push(trimmed.to_string());
            }
        }
    }

    Ok(items.join("\n\n"))
}

/// Reads a Go file from disk and extracts its type skeleton.
pub fn extract_go_file_skeleton(path: &Path) -> Result<String, DistillError> {
    let content = std::fs::read_to_string(path).map_err(DistillError::Io)?;
    extract_go_skeleton(&content)
}

/// Reads a Go file from disk and extracts its type skeleton with options.
pub fn extract_go_file_skeleton_with_options(
    path: &Path,
    include_private: bool,
    omit_tests: bool,
) -> Result<String, DistillError> {
    let content = std::fs::read_to_string(path).map_err(DistillError::Io)?;
    extract_go_skeleton_with_options(&content, include_private, omit_tests)
}

fn format_go_node(
    node: tree_sitter::Node<'_>,
    src: &str,
    include_private: bool,
    omit_tests: bool,
) -> Option<String> {
    match node.kind() {
        "package_clause" | "const_declaration" | "var_declaration" | "type_declaration" => {
            let text = &src[node.byte_range()];
            Some(text.to_string())
        }
        "function_declaration" => format_function_node(node, src, include_private, omit_tests),
        "method_declaration" => format_method_node(node, src, include_private, omit_tests),
        _ => None,
    }
}

fn format_function_node(
    node: tree_sitter::Node<'_>,
    src: &str,
    include_private: bool,
    omit_tests: bool,
) -> Option<String> {
    let name_node = node.child_by_field_name("name")?;
    let name = &src[name_node.byte_range()];
    if !include_private && is_unexported(name) {
        return None;
    }

    let body_node = node.child_by_field_name("body");
    let sig_end = body_node
        .map(|b| b.start_byte())
        .unwrap_or_else(|| node.end_byte());
    let sig = src[node.start_byte()..sig_end].trim_end();
    if omit_tests && (is_go_test_function(name) || has_go_test_signature(sig)) {
        return None;
    }

    Some(format!("{};", sig))
}

fn format_method_node(
    node: tree_sitter::Node<'_>,
    src: &str,
    include_private: bool,
    omit_tests: bool,
) -> Option<String> {
    let name_node = node.child_by_field_name("name")?;
    let name = &src[name_node.byte_range()];
    if !include_private && is_unexported(name) {
        return None;
    }

    let body_node = node.child_by_field_name("body");
    let sig_end = body_node
        .map(|b| b.start_byte())
        .unwrap_or_else(|| node.end_byte());
    let sig = src[node.start_byte()..sig_end].trim_end();
    if omit_tests && (is_go_test_function(name) || has_go_test_signature(sig)) {
        return None;
    }

    Some(format!("{};", sig))
}

/// Returns true if the function name follows Go testing conventions (Test*, Benchmark*, Fuzz*, Example*).
pub fn is_go_test_function(name: &str) -> bool {
    let matches_prefix = |prefix: &str| {
        if let Some(rest) = name.strip_prefix(prefix) {
            rest.is_empty() || rest.chars().next().is_some_and(|c| !c.is_lowercase())
        } else {
            false
        }
    };
    matches_prefix("Test")
        || matches_prefix("Benchmark")
        || matches_prefix("Fuzz")
        || matches_prefix("Example")
}

/// Returns true if the function or method signature contains testing parameter types (*testing.T, *testing.B, *testing.F).
pub fn has_go_test_signature(sig: &str) -> bool {
    sig.contains("*testing.T")
        || sig.contains("*testing.B")
        || sig.contains("*testing.F")
        || sig.contains("testing.TB")
}

fn is_unexported(name: &str) -> bool {
    name.chars()
        .next()
        .map(|c| c.is_lowercase())
        .unwrap_or(false)
}

/// Output data extracted from Go source code for indexing.
#[derive(Debug, Default, Clone)]
pub struct GoIndexedData {
    pub symbols: Vec<SymbolDef>,
    pub contracts: Vec<ContractInfo>,
    pub routes: Vec<RouteBinding>,
}

/// Parses and indexes Go source code symbols, method sets, interface contracts, and HTTP routes.
pub fn index_go_ast(file_path: &str, content: &str) -> Result<GoIndexedData, DistillError> {
    index_go_ast_with_options(file_path, content, true)
}

/// Indexes Go source code extracting symbols, implicit interface satisfaction contracts, and routes.
pub fn index_go_ast_with_options(
    file_path: &str,
    content: &str,
    omit_tests: bool,
) -> Result<GoIndexedData, DistillError> {
    let p = Path::new(file_path);
    let opts = crate::indexer::go_graph::GoFactOptions {
        file_path: p,
        rel_path: p,
        package_path: None,
        omit_tests,
    };
    let facts = crate::indexer::go_graph::extract_go_file_facts(&opts, content)?;
    Ok(GoIndexedData {
        symbols: facts.symbols,
        contracts: facts.contracts,
        routes: facts.routes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_go_user_service() {
        let sample_go = r#"
package service

import (
    "context"
    "database/sql"
    "sync"
    "time"
)

type UserStatus string

const (
    StatusActive   UserStatus = "active"
    StatusInactive UserStatus = "inactive"
)

type User struct {
    ID        string     `json:"id" db:"id"`
    Email     string     `json:"email" db:"email"`
    Status    UserStatus `json:"status" db:"status"`
    CreatedAt time.Time  `json:"created_at" db:"created_at"`
}

type UserService struct {
    db   *sql.DB
    mu   sync.RWMutex
}

func NewUserService(db *sql.DB) *UserService {
    return &UserService{db: db}
}

func (s *UserService) GetUser(ctx context.Context, id string) (*User, error) {
    s.mu.RLock()
    defer s.mu.RUnlock()
    return nil, nil
}

func (s *UserService) internalHelper() {
    // hidden helper
}
"#;

        let skeleton = extract_go_skeleton(sample_go).unwrap();
        assert!(skeleton.contains("package service"));
        assert!(skeleton.contains("type UserStatus string"));
        assert!(skeleton.contains("StatusActive   UserStatus = \"active\""));
        assert!(skeleton.contains("`json:\"id\" db:\"id\"`"));
        assert!(skeleton.contains("func NewUserService(db *sql.DB) *UserService;"));
        assert!(skeleton.contains(
            "func (s *UserService) GetUser(ctx context.Context, id string) (*User, error);"
        ));
        assert!(!skeleton.contains("s.mu.RLock()"));

        // Test without private types/helpers
        let skeleton_public = extract_go_skeleton_with_options(sample_go, false, true).unwrap();
        assert!(!skeleton_public.contains("internalHelper"));
    }

    #[test]
    fn test_extract_go_skeleton_omit_tests() {
        let sample_go = r#"
package math

func Add(a, b int) int {
    return a + b
}

func helper(t *testing.T) {
    t.Helper()
}

func TestAdd(t *testing.T) {
    if Add(1, 2) != 3 {
        t.Fail()
    }
}

func BenchmarkAdd(b *testing.B) {
    for i := 0; i < b.N; i++ {
        Add(1, 2)
    }
}

func FuzzAdd(f *testing.F) {
    f.Fuzz(func(t *testing.T, a, b int) {
        Add(a, b)
    })
}

func ExampleAdd() {
    fmt.Println(Add(1, 2))
}

type Suite struct {}

func (s *Suite) TestUser(t *testing.T) {
    s.run()
}

func (s *Suite) BenchmarkOperation(b *testing.B) {
    s.bench()
}
"#;

        // With omit_tests = true (default)
        let omitted = extract_go_skeleton_with_options(sample_go, true, true).unwrap();
        assert!(omitted.contains("func Add(a, b int) int;"));
        assert!(omitted.contains("type Suite struct"));
        assert!(!omitted.contains("TestAdd"));
        assert!(!omitted.contains("BenchmarkAdd"));
        assert!(!omitted.contains("FuzzAdd"));
        assert!(!omitted.contains("ExampleAdd"));
        assert!(!omitted.contains("helper"));
        assert!(!omitted.contains("TestUser"));
        assert!(!omitted.contains("BenchmarkOperation"));

        // With omit_tests = false
        let retained = extract_go_skeleton_with_options(sample_go, true, false).unwrap();
        assert!(retained.contains("func Add(a, b int) int;"));
        assert!(retained.contains("func helper(t *testing.T);"));
        assert!(retained.contains("func TestAdd(t *testing.T);"));
        assert!(retained.contains("func BenchmarkAdd(b *testing.B);"));
        assert!(retained.contains("func FuzzAdd(f *testing.F);"));
        assert!(retained.contains("func ExampleAdd();"));
        assert!(retained.contains("func (s *Suite) TestUser(t *testing.T);"));
        assert!(retained.contains("func (s *Suite) BenchmarkOperation(b *testing.B);"));

        // Helper functions
        assert!(is_go_test_function("TestFoo"));
        assert!(is_go_test_function("Test_Bar"));
        assert!(is_go_test_function("BenchmarkRun"));
        assert!(is_go_test_function("FuzzTarget"));
        assert!(is_go_test_function("ExamplePrint"));
        assert!(!is_go_test_function("TestingHelper"));
        assert!(!is_go_test_function("Add"));

        assert!(has_go_test_signature("func foo(t *testing.T)"));
        assert!(has_go_test_signature("func foo(b *testing.B)"));
        assert!(has_go_test_signature("func foo(f *testing.F)"));
        assert!(has_go_test_signature("func foo(tb testing.TB)"));
        assert!(!has_go_test_signature("func foo(x int)"));
    }

    #[test]
    fn test_go_distill_throughput_and_token_reduction() {
        let mut large_go = String::from("package monolithic\n\n");
        for i in 0..100 {
            large_go.push_str(&format!(
                r#"
type Handler{i} struct {{
    ID   int64  `json:"id" db:"id"`
    Name string `json:"name"`
}}

func (h *Handler{i}) Process(ctx context.Context, input string) (string, error) {{
    // Simulating verbose Go error cascades, mutex locks, defer statements
    if err := ctx.Err(); err != nil {{
        return "", fmt.Errorf("context canceled: %w", err)
    }}
    if input == "" {{
        return "", fmt.Errorf("empty input received for handler %d", h.ID)
    }}
    for idx := 0; idx < 100; idx++ {{
        if idx % 2 == 0 {{
            println("step a", idx)
        }} else {{
            println("step b", idx)
        }}
    }}
    res := fmt.Sprintf("result_%d_%s", h.ID, input)
    if len(res) > 1000 {{
        return "", fmt.Errorf("output buffer overflow")
    }}
    return res, nil
}}
"#
            ));
        }

        let line_count = large_go.lines().count();
        assert!(line_count > 1000);

        let start = std::time::Instant::now();
        let skeleton = extract_go_skeleton(&large_go).unwrap();
        let duration = start.elapsed();

        // Throughput target: sub-millisecond in release, well within threshold in debug
        assert!(
            duration.as_millis() < 100,
            "Distillation took too long: {:?}",
            duration
        );

        // Token reduction > 70%
        let reduction = (large_go.len() - skeleton.len()) as f64 / large_go.len() as f64 * 100.0;
        assert!(
            reduction > 70.0,
            "Expected >70% reduction, got {:.1}%",
            reduction
        );
        assert!(skeleton.contains("type Handler0 struct"));
        assert!(skeleton.contains(
            "func (h *Handler0) Process(ctx context.Context, input string) (string, error);"
        ));
        assert!(!skeleton.contains("println(idx)"));
    }

    #[test]
    fn test_index_go_ast_implicit_interfaces_and_routes() {
        let go_code = r#"
package service

type UserRepository interface {
    GetUser(ctx context.Context, id string) (*User, error)
}

type User struct {
    ID string `json:"id"`
}

type UserService struct {}

func (s *UserService) GetUser(ctx context.Context, id string) (*User, error) {
    return nil, nil
}

func RegisterRoutes(r *gin.Engine, s *UserService) {
    r.GET("/api/v1/users/:id", s.GetUser)
}
"#;

        let data = index_go_ast("internal/service/user.go", go_code).unwrap();
        assert!(
            data.symbols
                .iter()
                .any(|s| s.name == "UserRepository" && s.kind == "interface")
        );
        assert!(
            data.symbols
                .iter()
                .any(|s| s.name == "UserService" && s.kind == "struct")
        );
        assert!(
            data.symbols
                .iter()
                .any(|s| s.name == "User" && s.kind == "struct")
        );

        // Check implicit interface satisfaction: *UserService satisfies UserRepository
        assert_eq!(data.contracts.len(), 1);
        let contract = &data.contracts[0];
        assert_eq!(contract.trait_name, "UserRepository");
        assert_eq!(contract.target_type, "*UserService");
        assert!(!contract.methods.is_empty());

        // Check routes
        assert_eq!(data.routes.len(), 1);
        assert_eq!(data.routes[0].route_path, "/api/v1/users/:id");
        assert_eq!(data.routes[0].handler_or_caller, "s.GetUser");
    }
}
