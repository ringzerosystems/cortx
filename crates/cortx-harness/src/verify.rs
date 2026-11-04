//! Pillar 3: Bounded Model Checking via Kani.
//!
//! Provides bounded model checking runners using `cargo kani`, parsing of JSON SMT
//! counterexamples into concrete Rust initializers, and starter proof harness scaffolding.

use std::path::Path;
use std::process::Command;

use cortx_core::config::VerifyConfig;
use serde::{Deserialize, Serialize};

/// Strongly typed errors produced during formal model checking and verification.
#[derive(thiserror::Error, Debug)]
pub enum VerifyError {
    /// `cargo-kani` binary is not installed or not discoverable in PATH.
    #[error(
        "cargo-kani is not installed or not in PATH. Install via: cargo install --locked kani-verifier && cargo kani setup"
    )]
    KaniNotInstalled,
    /// Underlying command execution failed or returned a non-zero exit status.
    #[error("Kani execution failed (exit code {code:?}): {details}")]
    ExecutionFailed {
        /// Process exit code if available.
        code: Option<i32>,
        /// Stderr / stdout diagnostics from kani.
        details: String,
    },
    /// Failed to parse JSON output produced by `cargo kani`.
    #[error("failed to parse Kani JSON output: {source}")]
    JsonParse {
        /// Underlying JSON parsing error.
        #[source]
        source: serde_json::Error,
        /// Raw output received from tool.
        raw_output: String,
    },
    /// Model checking encountered verification failures / property violations.
    #[error("verification checks failed: {failed} of {total} checks failed")]
    VerificationFailed {
        /// Number of failed checks.
        failed: usize,
        /// Total checks executed.
        total: usize,
        /// Counterexamples extracted from solver trace.
        counterexamples: Vec<KaniCounterexample>,
    },
    /// Underlying I/O error during execution.
    #[error("I/O error during verification: {0}")]
    Io(#[from] std::io::Error),
}

/// Options configuring bounded model checking execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyOptions {
    /// Unwind depth limit for loops and recursions.
    pub unwind: usize,
    /// SAT/SMT solver backend (e.g., "kissat", "cadical", "z3").
    pub solver: String,
    /// Target proof harness name (optional filter).
    pub harness: Option<String>,
    /// Timeout in seconds.
    pub timeout_seconds: u64,
    /// Specific target file or directory override.
    pub target_file: Option<String>,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        Self {
            unwind: 4,
            solver: "kissat".to_string(),
            harness: None,
            timeout_seconds: 60,
            target_file: None,
        }
    }
}

impl From<&VerifyConfig> for VerifyOptions {
    fn from(config: &VerifyConfig) -> Self {
        Self {
            unwind: config.default_unwind,
            solver: config.solver.clone(),
            harness: None,
            timeout_seconds: 60,
            target_file: None,
        }
    }
}

/// Options for generating `#[kani::proof]` harness scaffolding stubs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KaniProofOptions {
    /// Unwind bound for the proof harness.
    pub unwind: usize,
    /// SAT/SMT solver annotation.
    pub solver: String,
    /// Precondition assumptions (`kani::assume(...)`).
    pub assumptions: Vec<String>,
    /// Function or method under verification.
    pub function_name: Option<String>,
}

impl Default for KaniProofOptions {
    fn default() -> Self {
        Self {
            unwind: 4,
            solver: "kissat".to_string(),
            assumptions: Vec::new(),
            function_name: None,
        }
    }
}

/// Variable assignment extracted from an SMT solver counterexample trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterexampleVariable {
    /// Variable identifier in the proof harness.
    pub name: String,
    /// Inferred or declared type name if available.
    pub type_name: Option<String>,
    /// Concrete value assigned by the solver.
    pub value: String,
}

/// Counterexample discovered by bounded model checking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KaniCounterexample {
    /// Description of the violated check or invariant.
    pub description: String,
    /// Property identifier (e.g. "assertion.1").
    pub property: String,
    /// Source file path where violation occurred.
    pub file: Option<String>,
    /// Source line number.
    pub line: Option<usize>,
    /// Source column number.
    pub col: Option<usize>,
    /// Concrete variable assignments from solver trace.
    pub variables: Vec<CounterexampleVariable>,
    /// Synthesized Rust initializer code reproducing the violation.
    pub concrete_initializer: String,
}

/// Summary report of bounded model checking execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KaniVerificationResult {
    /// True if all checks passed with zero counterexamples.
    pub success: bool,
    /// Total number of checks evaluated.
    pub total_checks: usize,
    /// Checks that passed.
    pub passed_checks: usize,
    /// Checks that failed with counterexamples.
    pub failed_checks: usize,
    /// Checks proven unreachable.
    pub unreachable_checks: usize,
    /// Discovered counterexamples.
    pub counterexamples: Vec<KaniCounterexample>,
    /// Raw output captured from tool.
    pub raw_output: String,
}

/// Parses Kani's JSON output into structured verification results.
pub fn parse_kani_json(json_str: &str) -> Result<KaniVerificationResult, VerifyError> {
    let value: serde_json::Value =
        serde_json::from_str(json_str).map_err(|source| VerifyError::JsonParse {
            source,
            raw_output: json_str.to_string(),
        })?;

    let checks_slice = if let Some(arr) = value.as_array() {
        arr.as_slice()
    } else if let Some(arr) = value.get("checks").and_then(|c| c.as_array()) {
        arr.as_slice()
    } else if let Some(arr) = value.get("results").and_then(|r| r.as_array()) {
        arr.as_slice()
    } else {
        &[]
    };

    let mut total_checks = 0;
    let mut passed_checks = 0;
    let mut failed_checks = 0;
    let mut unreachable_checks = 0;
    let mut counterexamples = Vec::new();

    for check in checks_slice {
        total_checks += 1;
        let status = check
            .get("status")
            .or_else(|| check.get("result"))
            .and_then(|s| s.as_str())
            .unwrap_or("UNKNOWN")
            .to_uppercase();

        match status.as_str() {
            "SUCCESS" | "PASSED" | "SATISFIED" => {
                passed_checks += 1;
            }
            "UNREACHABLE" => {
                unreachable_checks += 1;
            }
            "FAILURE" | "FAILED" | "VIOLATED" => {
                failed_checks += 1;
                let desc = check
                    .get("description")
                    .or_else(|| check.get("message"))
                    .and_then(|d| d.as_str())
                    .unwrap_or("unnamed verification check")
                    .to_string();

                let prop = check
                    .get("property")
                    .or_else(|| check.get("id"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("property")
                    .to_string();

                let (file, line, col) = parse_location(check.get("location"));
                let variables = extract_trace_variables(check);

                let mut example = KaniCounterexample {
                    description: desc,
                    property: prop,
                    file,
                    line,
                    col,
                    variables,
                    concrete_initializer: String::new(),
                };
                example.concrete_initializer = synthesize_counterexample_initializer(&example);
                counterexamples.push(example);
            }
            _ => {}
        }
    }

    let top_status = value
        .get("status")
        .and_then(|s| s.as_str())
        .map(|s| s.to_uppercase());

    let success = if let Some(status) = top_status {
        status == "SUCCESS" && failed_checks == 0
    } else {
        failed_checks == 0 && total_checks > 0
    };

    Ok(KaniVerificationResult {
        success,
        total_checks,
        passed_checks,
        failed_checks,
        unreachable_checks,
        counterexamples,
        raw_output: json_str.to_string(),
    })
}

fn parse_location(
    loc: Option<&serde_json::Value>,
) -> (Option<String>, Option<usize>, Option<usize>) {
    let Some(loc) = loc else {
        return (None, None, None);
    };

    if let Some(s) = loc.as_str() {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() >= 3 {
            let file = Some(parts[0].to_string());
            let line = parts[1].parse::<usize>().ok();
            let col = parts[2].parse::<usize>().ok();
            return (file, line, col);
        } else if parts.len() == 2 {
            let file = Some(parts[0].to_string());
            let line = parts[1].parse::<usize>().ok();
            return (file, line, None);
        }
        return (Some(s.to_string()), None, None);
    }

    let file = loc
        .get("file")
        .or_else(|| loc.get("path"))
        .and_then(|f| f.as_str())
        .map(|f| f.to_string());
    let line = loc.get("line").and_then(|l| l.as_u64()).map(|l| l as usize);
    let col = loc
        .get("col")
        .or_else(|| loc.get("column"))
        .and_then(|c| c.as_u64())
        .map(|c| c as usize);

    (file, line, col)
}

fn extract_trace_variables(check: &serde_json::Value) -> Vec<CounterexampleVariable> {
    let mut vars = Vec::new();
    let trace_slice = check
        .get("trace")
        .or_else(|| check.get("counterexample"))
        .or_else(|| check.get("steps"))
        .and_then(|t| t.as_array());

    let Some(trace) = trace_slice else {
        return vars;
    };

    for item in trace {
        let name = item
            .get("name")
            .or_else(|| item.get("variable"))
            .and_then(|v| v.as_str());

        let value = item
            .get("value")
            .or_else(|| item.get("val"))
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                other => other.to_string(),
            });

        if let (Some(name), Some(value)) = (name, value) {
            let type_name = item
                .get("type")
                .or_else(|| item.get("type_name"))
                .and_then(|t| t.as_str())
                .map(|t| t.to_string());

            vars.push(CounterexampleVariable {
                name: name.to_string(),
                type_name,
                value,
            });
        }
    }

    vars
}

/// Synthesizes concrete Rust variable declarations and an executable reproducer test.
pub fn synthesize_counterexample_initializer(counterexample: &KaniCounterexample) -> String {
    let mut code = String::new();
    let loc_comment = match (&counterexample.file, counterexample.line) {
        (Some(f), Some(l)) => format!(" // Failed at {}:{}", f, l),
        _ => String::new(),
    };

    code.push_str(&format!(
        "// Violates: {}{}\n",
        counterexample.description, loc_comment
    ));
    code.push_str("#[test]\n");
    code.push_str("fn test_kani_counterexample_reproducer() {\n");

    if counterexample.variables.is_empty() {
        code.push_str("    // Counterexample trace contained no concrete variable assignments\n");
    } else {
        for var in &counterexample.variables {
            let type_annotation = var
                .type_name
                .as_deref()
                .map(|t| format!(": {}", t))
                .unwrap_or_default();
            code.push_str(&format!(
                "    let {}{} = {};\n",
                var.name, type_annotation, var.value
            ));
        }
    }

    code.push_str(&format!(
        "    // Expected violation: {}\n",
        counterexample.description
    ));
    code.push_str("}\n");

    code
}

/// Generates a starter `#[kani::proof]` verification harness stub.
pub fn scaffold_kani_proof(symbol_name: &str, options: &KaniProofOptions) -> String {
    let clean_symbol = symbol_name.trim();
    let fn_call = options
        .function_name
        .as_deref()
        .unwrap_or("verify_properties");

    let mut out = String::new();
    out.push_str("#[cfg(kani)]\n");
    out.push_str("mod verification {\n");
    out.push_str("    use super::*;\n\n");
    out.push_str("    #[kani::proof]\n");
    out.push_str(&format!("    #[kani::unwind({})]\n", options.unwind));
    out.push_str(&format!("    #[kani::solver({})]\n", options.solver));
    out.push_str(&format!("    fn {}() {{\n", fn_call));
    out.push_str(&format!(
        "        let mut subject = {}::default();\n",
        clean_symbol
    ));

    for assumption in &options.assumptions {
        out.push_str(&format!("        kani::assume({});\n", assumption));
    }
    if options.assumptions.is_empty() {
        out.push_str("        // kani::assume(subject.is_valid());\n");
    }

    out.push_str("        // Exercise operations under test:\n");
    out.push_str("        // subject.step();\n");
    out.push_str(
        "        // assert!(subject.invariant_holds(), \"Property invariant breached\");\n",
    );
    out.push_str("    }\n");
    out.push_str("}\n");

    out
}

/// Executes bounded model checking via `cargo kani`.
pub fn run_kani_verify(
    options: &VerifyOptions,
    working_dir: Option<&Path>,
) -> Result<KaniVerificationResult, VerifyError> {
    let mut cmd = Command::new("cargo");
    cmd.arg("kani")
        .arg("--output-format")
        .arg("json")
        .arg(format!("--default-unwind={}", options.unwind))
        .arg(format!("--solver={}", options.solver));

    if let Some(ref harness) = options.harness {
        cmd.arg(format!("--harness={}", harness));
    }

    if let Some(ref target) = options.target_file {
        cmd.arg(target);
    }

    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }

    let output = match cmd.output() {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(VerifyError::KaniNotInstalled);
        }
        Err(e) => return Err(VerifyError::Io(e)),
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() && stdout.trim().is_empty() {
        return Err(VerifyError::ExecutionFailed {
            code: output.status.code(),
            details: if stderr.is_empty() {
                format!("cargo-kani exited with code {:?}", output.status.code())
            } else {
                stderr
            },
        });
    }

    let result = parse_kani_json(&stdout)?;

    if !result.success && result.failed_checks > 0 {
        return Err(VerifyError::VerificationFailed {
            failed: result.failed_checks,
            total: result.total_checks,
            counterexamples: result.counterexamples,
        });
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_kani_success_json() {
        let json = r#"{
            "status": "SUCCESS",
            "checks": [
                {
                    "description": "bounds check",
                    "status": "SUCCESS",
                    "property": "bounds.1"
                },
                {
                    "description": "arithmetic overflow",
                    "status": "SUCCESS",
                    "property": "overflow.1"
                }
            ]
        }"#;

        let result = parse_kani_json(json).expect("should parse valid kani json");
        assert!(result.success);
        assert_eq!(result.total_checks, 2);
        assert_eq!(result.passed_checks, 2);
        assert_eq!(result.failed_checks, 0);
        assert!(result.counterexamples.is_empty());
    }

    #[test]
    fn test_parse_kani_failure_with_counterexample() {
        let json = r#"{
            "status": "FAILURE",
            "checks": [
                {
                    "description": "assertion failed: idx < len",
                    "status": "FAILURE",
                    "property": "assertion.1",
                    "location": {
                        "file": "src/ring_buffer.rs",
                        "line": 42,
                        "col": 9
                    },
                    "trace": [
                        { "name": "idx", "type": "usize", "value": "16" },
                        { "name": "len", "type": "usize", "value": "16" }
                    ]
                }
            ]
        }"#;

        let result = parse_kani_json(json).expect("should parse failure json");
        assert!(!result.success);
        assert_eq!(result.total_checks, 1);
        assert_eq!(result.failed_checks, 1);
        assert_eq!(result.counterexamples.len(), 1);

        let ce = &result.counterexamples[0];
        assert_eq!(ce.description, "assertion failed: idx < len");
        assert_eq!(ce.file.as_deref(), Some("src/ring_buffer.rs"));
        assert_eq!(ce.line, Some(42));
        assert_eq!(ce.variables.len(), 2);
        assert_eq!(ce.variables[0].name, "idx");
        assert_eq!(ce.variables[0].value, "16");

        assert!(ce.concrete_initializer.contains("let idx: usize = 16;"));
        assert!(ce.concrete_initializer.contains("let len: usize = 16;"));
    }

    #[test]
    fn test_parse_kani_array_format() {
        let json = r#"[
            {
                "description": "division by zero",
                "status": "FAILURE",
                "property": "div.0",
                "location": "src/math.rs:10:4",
                "counterexample": [
                    { "variable": "divisor", "val": "0", "type_name": "i32" }
                ]
            }
        ]"#;

        let result = parse_kani_json(json).expect("should parse array format");
        assert!(!result.success);
        assert_eq!(result.failed_checks, 1);
        let ce = &result.counterexamples[0];
        assert_eq!(ce.file.as_deref(), Some("src/math.rs"));
        assert_eq!(ce.line, Some(10));
        assert_eq!(ce.variables[0].name, "divisor");
        assert_eq!(ce.variables[0].value, "0");
    }

    #[test]
    fn test_synthesize_counterexample_initializer() {
        let ce = KaniCounterexample {
            description: "capacity exceeded".to_string(),
            property: "cap.1".to_string(),
            file: Some("src/buffer.rs".to_string()),
            line: Some(88),
            col: Some(5),
            variables: vec![
                CounterexampleVariable {
                    name: "capacity".to_string(),
                    type_name: Some("usize".to_string()),
                    value: "1024".to_string(),
                },
                CounterexampleVariable {
                    name: "requested".to_string(),
                    type_name: Some("usize".to_string()),
                    value: "2048".to_string(),
                },
            ],
            concrete_initializer: String::new(),
        };

        let code = synthesize_counterexample_initializer(&ce);
        assert!(code.contains("let capacity: usize = 1024;"));
        assert!(code.contains("let requested: usize = 2048;"));
        assert!(code.contains("fn test_kani_counterexample_reproducer()"));
        assert!(code.contains("Violates: capacity exceeded // Failed at src/buffer.rs:88"));
    }

    #[test]
    fn test_scaffold_kani_proof_defaults() {
        let options = KaniProofOptions::default();
        let code = scaffold_kani_proof("RingBuffer", &options);

        assert!(code.contains("#[cfg(kani)]"));
        assert!(code.contains("#[kani::proof]"));
        assert!(code.contains("#[kani::unwind(4)]"));
        assert!(code.contains("#[kani::solver(kissat)]"));
        assert!(code.contains("let mut subject = RingBuffer::default();"));
    }

    #[test]
    fn test_scaffold_kani_proof_custom_options() {
        let options = KaniProofOptions {
            unwind: 8,
            solver: "cadical".to_string(),
            assumptions: vec!["subject.capacity() > 0".to_string()],
            function_name: Some("verify_ring_buffer_invariants".to_string()),
        };
        let code = scaffold_kani_proof("RingBuffer", &options);

        assert!(code.contains("#[kani::unwind(8)]"));
        assert!(code.contains("#[kani::solver(cadical)]"));
        assert!(code.contains("fn verify_ring_buffer_invariants()"));
        assert!(code.contains("kani::assume(subject.capacity() > 0);"));
    }
}
