//! Pillar 2: Differential Fuzzing Oracle & Cross-Boundary Contracts.
//!
//! Provides shadow trait differential fuzzing (reference vs candidate), sequence shrinking,
//! cross-boundary Serde <-> TypeScript <-> Go contract validation (int53 safe integer bounds,
//! casing divergence, null vs empty slice invariants), and standalone reproducer export.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Maximum safe integer in JavaScript / TypeScript (2^53 - 1).
pub const JS_MAX_SAFE_INT: i64 = 9_007_199_254_740_991;

/// Minimum safe integer in JavaScript / TypeScript (-(2^53 - 1)).
pub const JS_MIN_SAFE_INT: i64 = -9_007_199_254_740_991;

/// Strongly typed errors produced during differential fuzzing runs.
#[derive(thiserror::Error, Debug)]
pub enum DiffFuzzError {
    /// Divergence observed between reference and candidate outputs.
    #[error("differential divergence at iteration {iteration}: reference={reference_output}, candidate={candidate_output}")]
    Divergence {
        /// Iteration index where divergence occurred.
        iteration: u64,
        /// Debug string of reference output.
        reference_output: String,
        /// Debug string of candidate output.
        candidate_output: String,
    },
    /// Cross-boundary contract invariant violation.
    #[error("cross-boundary contract violation at iteration {iteration}: {details}")]
    ContractViolation {
        /// Iteration index where violation occurred.
        iteration: u64,
        /// Formatted details of contract violations.
        details: String,
    },
    /// Underlying JSON serialization error.
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    /// Underlying I/O error during reproducer export.
    #[error("I/O error during reproducer export: {0}")]
    Io(#[from] std::io::Error),
}

/// Trait for a target in differential testing comparing reference vs candidate implementations.
pub trait ShadowTarget<I, O> {
    /// Executes the reference implementation.
    fn reference(&mut self, input: &I) -> O;
    /// Executes the candidate implementation.
    fn candidate(&mut self, input: &I) -> O;
}

/// Options configuring differential fuzzing runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFuzzOptions {
    /// Number of iterations to execute.
    pub iterations: u64,
    /// Deterministic PRNG seed.
    pub seed: u64,
    /// Automatically shrink failing input on divergence.
    pub shrink_on_failure: bool,
}

impl Default for DiffFuzzOptions {
    fn default() -> Self {
        Self {
            iterations: 1_000,
            seed: 0x1337_C0DE_CAFE_BABE,
            shrink_on_failure: true,
        }
    }
}

/// Differential mismatch report capturing input and divergent outputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffMismatch<I, O> {
    /// Iteration where divergence occurred.
    pub iteration: u64,
    /// Original input that triggered divergence.
    pub input: I,
    /// Output from reference implementation.
    pub reference_output: O,
    /// Output from candidate implementation.
    pub candidate_output: O,
    /// Shrunk minimal input if shrinking was enabled and successful.
    pub shrunk_input: Option<I>,
}

/// Differential fuzzing summary metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFuzzSummary {
    /// Total number of iterations executed.
    pub iterations_executed: u64,
    /// Total iterations where outputs matched.
    pub matches: u64,
}

/// Executes differential fuzzing comparing reference and candidate functions.
pub fn run_diff_fuzz<I, O, FGen, FRef, FCand>(
    options: &DiffFuzzOptions,
    mut generator: FGen,
    mut reference: FRef,
    mut candidate: FCand,
) -> Result<DiffFuzzSummary, Box<DiffMismatch<I, O>>>
where
    I: Clone + core::fmt::Debug + PartialEq,
    O: Clone + core::fmt::Debug + PartialEq,
    FGen: FnMut(u64) -> I,
    FRef: FnMut(&I) -> O,
    FCand: FnMut(&I) -> O,
{
    for i in 0..options.iterations {
        let input = generator(i);
        let ref_out = reference(&input);
        let cand_out = candidate(&input);
        if ref_out != cand_out {
            return Err(Box::new(DiffMismatch {
                iteration: i,
                input,
                reference_output: ref_out,
                candidate_output: cand_out,
                shrunk_input: None,
            }));
        }
    }
    Ok(DiffFuzzSummary {
        iterations_executed: options.iterations,
        matches: options.iterations,
    })
}

/// Executes differential fuzzing over sequences with automated delta-debugging shrinking.
pub fn run_diff_fuzz_sequence<Op, O, FGen, FRef, FCand>(
    options: &DiffFuzzOptions,
    mut generator: FGen,
    mut reference: FRef,
    mut candidate: FCand,
) -> Result<DiffFuzzSummary, Box<DiffMismatch<Vec<Op>, O>>>
where
    Op: Clone + core::fmt::Debug + PartialEq,
    O: Clone + core::fmt::Debug + PartialEq,
    FGen: FnMut(u64) -> Vec<Op>,
    FRef: FnMut(&[Op]) -> O,
    FCand: FnMut(&[Op]) -> O,
{
    for i in 0..options.iterations {
        let input = generator(i);
        let ref_out = reference(&input);
        let cand_out = candidate(&input);
        if ref_out != cand_out {
            let shrunk = if options.shrink_on_failure {
                Some(shrink_sequence(&input, |sub| {
                    reference(sub) != candidate(sub)
                }))
            } else {
                None
            };
            return Err(Box::new(DiffMismatch {
                iteration: i,
                input,
                reference_output: ref_out,
                candidate_output: cand_out,
                shrunk_input: shrunk,
            }));
        }
    }
    Ok(DiffFuzzSummary {
        iterations_executed: options.iterations,
        matches: options.iterations,
    })
}

/// Executes differential fuzzing against a stateful `ShadowTarget`.
pub fn run_shadow_diff_fuzz<T, I, O, FGen>(
    target: &mut T,
    options: &DiffFuzzOptions,
    mut generator: FGen,
) -> Result<DiffFuzzSummary, Box<DiffMismatch<I, O>>>
where
    T: ShadowTarget<I, O>,
    I: Clone + core::fmt::Debug + PartialEq,
    O: Clone + core::fmt::Debug + PartialEq,
    FGen: FnMut(u64) -> I,
{
    for i in 0..options.iterations {
        let input = generator(i);
        let ref_out = target.reference(&input);
        let cand_out = target.candidate(&input);
        if ref_out != cand_out {
            return Err(Box::new(DiffMismatch {
                iteration: i,
                input,
                reference_output: ref_out,
                candidate_output: cand_out,
                shrunk_input: None,
            }));
        }
    }
    Ok(DiffFuzzSummary {
        iterations_executed: options.iterations,
        matches: options.iterations,
    })
}

/// Shrinks a failing sequence using delta-debugging chunk and element removal.
pub fn shrink_sequence<T, F>(sequence: &[T], mut is_failing: F) -> Vec<T>
where
    T: Clone,
    F: FnMut(&[T]) -> bool,
{
    if sequence.is_empty() {
        return Vec::new();
    }
    let mut current = sequence.to_vec();
    let mut step = current.len() / 2;

    while step > 0 {
        let mut i = 0;
        let mut improved = false;
        while i + step <= current.len() {
            let mut candidate = current.clone();
            candidate.drain(i..i + step);
            if !candidate.is_empty() && is_failing(&candidate) {
                current = candidate;
                improved = true;
                break;
            }
            i += 1;
        }
        if !improved {
            step /= 2;
        }
    }

    let mut i = 0;
    while i < current.len() {
        if current.len() <= 1 {
            break;
        }
        let mut candidate = current.clone();
        candidate.remove(i);
        if is_failing(&candidate) {
            current = candidate;
        } else {
            i += 1;
        }
    }

    current
}

/// Casing style for cross-boundary JSON keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CasingStyle {
    /// camelCase (e.g. `userId`, `accountStatus`)
    CamelCase,
    /// snake_case (e.g. `user_id`, `account_status`)
    SnakeCase,
    /// PascalCase (e.g. `UserId`, `AccountStatus`)
    PascalCase,
    /// kebab-case (e.g. `user-id`, `account-status`)
    KebabCase,
}

impl CasingStyle {
    /// Validates whether a string matches this casing style.
    pub fn matches(&self, s: &str) -> bool {
        if s.is_empty() {
            return true;
        }
        match self {
            Self::CamelCase => {
                let first = s.chars().next().unwrap_or(' ');
                if !first.is_ascii_lowercase() {
                    return false;
                }
                !s.contains('_') && !s.contains('-')
            }
            Self::SnakeCase => {
                if s.contains('-') {
                    return false;
                }
                s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            }
            Self::PascalCase => {
                let first = s.chars().next().unwrap_or(' ');
                if !first.is_ascii_uppercase() {
                    return false;
                }
                !s.contains('_') && !s.contains('-')
            }
            Self::KebabCase => {
                if s.contains('_') {
                    return false;
                }
                s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            }
        }
    }
}

/// Categorized cross-boundary contract violation kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContractViolationKind {
    /// Signed integer exceeds JavaScript 53-bit safe integer bounds.
    Int53Overflow {
        /// Actual integer value.
        value: i64,
        /// Maximum safe integer (9,007,199,254,740,991).
        max_safe: i64,
        /// Minimum safe integer (-9,007,199,254,740,991).
        min_safe: i64,
    },
    /// Unsigned integer exceeds JavaScript 53-bit safe integer bounds.
    Int53UnsignedOverflow {
        /// Actual unsigned integer value.
        value: u64,
        /// Maximum safe integer (9,007,199,254,740,991).
        max_safe: u64,
    },
    /// Field key violates expected casing convention.
    CasingDivergence {
        /// Key name.
        key: String,
        /// Expected casing convention.
        expected: CasingStyle,
    },
    /// Divergence between null and empty slice/array representations.
    NullSliceDivergence {
        /// Divergent field path.
        field: String,
        /// Structural discrepancy explanation.
        details: String,
    },
    /// Discrepancy in JSON value types between boundaries.
    TypeMismatch {
        /// Field path.
        field: String,
        /// Expected type name.
        expected: String,
        /// Actual observed type name.
        actual: String,
    },
}

/// Single detected cross-boundary contract violation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractViolation {
    /// Path within JSON payload (e.g. `users[0].account_balance`).
    pub path: String,
    /// Categorized violation kind.
    pub kind: ContractViolationKind,
    /// Detailed diagnostic message.
    pub message: String,
}

/// Recursively inspects a JSON value for integer values exceeding JS Int53 safe ranges.
pub fn check_int53_bounds(val: &serde_json::Value, path: &str) -> Vec<ContractViolation> {
    let mut violations = Vec::new();
    check_int53_recursive(val, path, &mut violations);
    violations
}

fn check_int53_recursive(
    val: &serde_json::Value,
    path: &str,
    violations: &mut Vec<ContractViolation>,
) {
    match val {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if !(JS_MIN_SAFE_INT..=JS_MAX_SAFE_INT).contains(&i) {
                    violations.push(ContractViolation {
                        path: path.to_string(),
                        kind: ContractViolationKind::Int53Overflow {
                            value: i,
                            max_safe: JS_MAX_SAFE_INT,
                            min_safe: JS_MIN_SAFE_INT,
                        },
                        message: format!(
                            "Integer value {i} at '{path}' exceeds JS safe integer bounds [{JS_MIN_SAFE_INT}, {JS_MAX_SAFE_INT}]"
                        ),
                    });
                }
            } else if let Some(u) = n.as_u64() {
                if u > JS_MAX_SAFE_INT as u64 {
                    violations.push(ContractViolation {
                        path: path.to_string(),
                        kind: ContractViolationKind::Int53UnsignedOverflow {
                            value: u,
                            max_safe: JS_MAX_SAFE_INT as u64,
                        },
                        message: format!(
                            "Unsigned integer {u} at '{path}' exceeds JS max safe integer {JS_MAX_SAFE_INT}"
                        ),
                    });
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for (idx, elem) in arr.iter().enumerate() {
                let elem_path = format!("{path}[{idx}]");
                check_int53_recursive(elem, &elem_path, violations);
            }
        }
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let field_path = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                check_int53_recursive(v, &field_path, violations);
            }
        }
        _ => {}
    }
}

/// Recursively inspects object keys in a JSON value against expected casing conventions.
pub fn check_casing(
    val: &serde_json::Value,
    expected: CasingStyle,
    path: &str,
) -> Vec<ContractViolation> {
    let mut violations = Vec::new();
    check_casing_recursive(val, expected, path, &mut violations);
    violations
}

fn check_casing_recursive(
    val: &serde_json::Value,
    expected: CasingStyle,
    path: &str,
    violations: &mut Vec<ContractViolation>,
) {
    match val {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let field_path = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                if !expected.matches(k) {
                    violations.push(ContractViolation {
                        path: field_path.clone(),
                        kind: ContractViolationKind::CasingDivergence {
                            key: k.clone(),
                            expected,
                        },
                        message: format!(
                            "Key '{k}' at '{field_path}' violates expected {expected:?} convention"
                        ),
                    });
                }
                check_casing_recursive(v, expected, &field_path, violations);
            }
        }
        serde_json::Value::Array(arr) => {
            for (idx, elem) in arr.iter().enumerate() {
                let elem_path = format!("{path}[{idx}]");
                check_casing_recursive(elem, expected, &elem_path, violations);
            }
        }
        _ => {}
    }
}

/// Recursively compares reference and candidate JSON values to flag null vs array divergences.
pub fn check_null_slice_divergence(
    reference: &serde_json::Value,
    candidate: &serde_json::Value,
    path: &str,
) -> Vec<ContractViolation> {
    let mut violations = Vec::new();
    check_null_slice_recursive(reference, candidate, path, &mut violations);
    violations
}

fn check_null_slice_recursive(
    reference: &serde_json::Value,
    candidate: &serde_json::Value,
    path: &str,
    violations: &mut Vec<ContractViolation>,
) {
    match (reference, candidate) {
        (serde_json::Value::Array(_), serde_json::Value::Null) => {
            violations.push(ContractViolation {
                path: path.to_string(),
                kind: ContractViolationKind::NullSliceDivergence {
                    field: path.to_string(),
                    details: "Reference is Array, but candidate is null".to_string(),
                },
                message: format!(
                    "Field '{path}' diverged: reference is Array ([]), candidate is null"
                ),
            });
        }
        (serde_json::Value::Null, serde_json::Value::Array(_)) => {
            violations.push(ContractViolation {
                path: path.to_string(),
                kind: ContractViolationKind::NullSliceDivergence {
                    field: path.to_string(),
                    details: "Reference is null, but candidate is Array".to_string(),
                },
                message: format!(
                    "Field '{path}' diverged: reference is null, candidate is Array ([])"
                ),
            });
        }
        (serde_json::Value::Object(ref_map), serde_json::Value::Object(cand_map)) => {
            for (k, ref_v) in ref_map {
                let field_path = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                if let Some(cand_v) = cand_map.get(k) {
                    check_null_slice_recursive(ref_v, cand_v, &field_path, violations);
                }
            }
        }
        (serde_json::Value::Array(ref_arr), serde_json::Value::Array(cand_arr)) => {
            for (i, (r, c)) in ref_arr.iter().zip(cand_arr.iter()).enumerate() {
                let elem_path = format!("{path}[{i}]");
                check_null_slice_recursive(r, c, &elem_path, violations);
            }
        }
        _ => {}
    }
}

/// Validates cross-boundary contract invariants across two JSON structures.
pub fn validate_cross_boundary_contract(
    reference: &serde_json::Value,
    candidate: &serde_json::Value,
    expected_casing: Option<CasingStyle>,
) -> Vec<ContractViolation> {
    let mut violations = Vec::new();
    violations.extend(check_int53_bounds(reference, "reference"));
    violations.extend(check_int53_bounds(candidate, "candidate"));
    if let Some(casing) = expected_casing {
        violations.extend(check_casing(reference, casing, "reference"));
        violations.extend(check_casing(candidate, casing, "candidate"));
    }
    violations.extend(check_null_slice_divergence(reference, candidate, "root"));
    violations
}

/// Options configuring cross-boundary contract differential fuzzing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossBoundaryOptions {
    /// Standard differential fuzzing options.
    pub diff_options: DiffFuzzOptions,
    /// Expected key casing convention.
    pub expected_casing: Option<CasingStyle>,
}

/// Report for a cross-boundary contract fuzzing failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrossBoundaryFailure<I> {
    /// Iteration where contract violation occurred.
    pub iteration: u64,
    /// Generating input.
    pub input: I,
    /// Serialized reference payload.
    pub reference_json: serde_json::Value,
    /// Serialized candidate payload.
    pub candidate_json: serde_json::Value,
    /// List of contract violations detected.
    pub violations: Vec<ContractViolation>,
}

/// Fuzzes cross-boundary serializers and checks contract rules.
pub fn run_cross_boundary_diff_fuzz<I, FGen, FRef, FCand>(
    options: &CrossBoundaryOptions,
    mut generator: FGen,
    mut reference_ser: FRef,
    mut candidate_ser: FCand,
) -> Result<DiffFuzzSummary, Box<CrossBoundaryFailure<I>>>
where
    I: Clone + core::fmt::Debug,
    FGen: FnMut(u64) -> I,
    FRef: FnMut(&I) -> serde_json::Value,
    FCand: FnMut(&I) -> serde_json::Value,
{
    for i in 0..options.diff_options.iterations {
        let input = generator(i);
        let ref_json = reference_ser(&input);
        let cand_json = candidate_ser(&input);
        let violations =
            validate_cross_boundary_contract(&ref_json, &cand_json, options.expected_casing);
        if !violations.is_empty() {
            return Err(Box::new(CrossBoundaryFailure {
                iteration: i,
                input,
                reference_json: ref_json,
                candidate_json: cand_json,
                violations,
            }));
        }
    }
    Ok(DiffFuzzSummary {
        iterations_executed: options.diff_options.iterations,
        matches: options.diff_options.iterations,
    })
}

/// Target language for standalone reproducer generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReproducerLanguage {
    /// Standalone Rust `#[test]` reproducer module.
    Rust,
    /// Standalone TypeScript / Vitest test file.
    TypeScript,
    /// Standalone Go `_test.go` test file.
    Go,
}

/// Parameter specification for standalone reproducer test synthesis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReproducerSpec<'a> {
    /// Target programming language.
    pub language: ReproducerLanguage,
    /// Test identifier name.
    pub test_name: &'a str,
    /// Serialized JSON string of the input triggering divergence.
    pub input_json: &'a str,
    /// Serialized JSON string of expected output (if any).
    pub expected_json: Option<&'a str>,
    /// Serialized JSON string of actual candidate output (if any).
    pub candidate_json: Option<&'a str>,
}

/// Generates a standalone reproducer test file as formatted source code.
pub fn export_reproducer_test(spec: &ReproducerSpec<'_>) -> String {
    let sanitized_name = spec
        .test_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();

    let expected = spec.expected_json.unwrap_or("{}");
    let actual = spec.candidate_json.unwrap_or("{}");

    match spec.language {
        ReproducerLanguage::Rust => format!(
            r##"#[cfg(test)]
mod reproducer_{sanitized_name} {{
    use serde_json::Value;

    #[test]
    fn test_{sanitized_name}() {{
        let input_raw = r#"{input}"#;
        let _input: Value = serde_json::from_str(input_raw).expect("valid input json");
        let expected_raw = r#"{expected}"#;
        let expected: Value = serde_json::from_str(expected_raw).expect("valid expected json");
        let actual_raw = r#"{actual}"#;
        let actual: Value = serde_json::from_str(actual_raw).expect("valid actual json");

        assert_eq!(actual, expected, "Differential divergence for test {sanitized_name}");
    }}
}}
"##,
            input = spec.input_json,
        ),
        ReproducerLanguage::TypeScript => format!(
            r#"import {{ describe, it, expect }} from "vitest";

describe("Reproducer: {sanitized_name}", () => {{
  it("reproduces differential mismatch", () => {{
    const input = JSON.parse(`{input}`);
    const expected = JSON.parse(`{expected}`);
    const actual = JSON.parse(`{actual}`);

    expect(actual).toEqual(expected);
  }});
}});
"#,
            input = spec.input_json,
        ),
        ReproducerLanguage::Go => format!(
            r#"package reproducer_test

import (
	"encoding/json"
	"reflect"
	"testing"
)

func TestReproducer_{sanitized_name}(t *testing.T) {{
	expectedJSON := `{expected}`
	actualJSON := `{actual}`

	var expected, actual any
	if err := json.Unmarshal([]byte(expectedJSON), &expected); err != nil {{
		t.Fatalf("failed to unmarshal expected: %v", err)
	}}
	if err := json.Unmarshal([]byte(actualJSON), &actual); err != nil {{
		t.Fatalf("failed to unmarshal actual: %v", err)
	}}

	if !reflect.DeepEqual(actual, expected) {{
		t.Fatalf("differential mismatch in {sanitized_name}:\ngot:  %v\nwant: %v", actual, expected)
	}}
}}
"#,
        ),
    }
}

/// Exports a standalone reproducer test directly to disk at the given path.
pub fn export_reproducer_to_file(path: &Path, spec: &ReproducerSpec<'_>) -> std::io::Result<()> {
    let content = export_reproducer_test(spec);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SimpleShadowTarget;

    impl ShadowTarget<i32, i32> for SimpleShadowTarget {
        fn reference(&mut self, input: &i32) -> i32 {
            input * 2
        }

        fn candidate(&mut self, input: &i32) -> i32 {
            if *input == 77 {
                999 // Bug on input 77
            } else {
                input * 2
            }
        }
    }

    #[test]
    fn test_shadow_target_matching() {
        let options = DiffFuzzOptions {
            iterations: 50,
            ..Default::default()
        };
        let res = run_diff_fuzz(&options, |i| i as i32, |x| x + 1, |x| x + 1);
        assert!(res.is_ok());
        let summary = res.unwrap();
        assert_eq!(summary.iterations_executed, 50);
        assert_eq!(summary.matches, 50);
    }

    #[test]
    fn test_shadow_target_divergence() {
        let options = DiffFuzzOptions {
            iterations: 100,
            ..Default::default()
        };
        let mut target = SimpleShadowTarget;
        let res = run_shadow_diff_fuzz(&mut target, &options, |i| i as i32);
        assert!(res.is_err());
        let mismatch = res.unwrap_err();
        assert_eq!(mismatch.iteration, 77);
        assert_eq!(mismatch.input, 77);
        assert_eq!(mismatch.reference_output, 154);
        assert_eq!(mismatch.candidate_output, 999);
    }

    #[test]
    fn test_sequence_shrinking() {
        let sequence = vec![1, 2, 3, 4, 42, 5, 6, 7, 8];
        let shrunk = shrink_sequence(&sequence, |slice| slice.contains(&42));
        assert_eq!(shrunk, vec![42]);
    }

    #[test]
    fn test_diff_fuzz_sequence_with_shrinking() {
        let options = DiffFuzzOptions {
            iterations: 5,
            shrink_on_failure: true,
            ..Default::default()
        };
        let res = run_diff_fuzz_sequence(
            &options,
            |_| vec![10, 20, 99, 30, 40],
            |slice| slice.iter().sum::<i32>(),
            |slice| {
                if slice.contains(&99) {
                    0
                } else {
                    slice.iter().sum::<i32>()
                }
            },
        );
        assert!(res.is_err());
        let failure = res.unwrap_err();
        assert_eq!(failure.shrunk_input, Some(vec![99]));
    }

    #[test]
    fn test_int53_safe_bounds_detection() {
        let safe_json = serde_json::json!({
            "id": 9_007_199_254_740_991_i64,
            "min_id": -9_007_199_254_740_991_i64,
            "small": 42
        });
        assert!(check_int53_bounds(&safe_json, "root").is_empty());

        let overflow_json = serde_json::json!({
            "too_large": 9_007_199_254_740_992_i64,
            "too_small": -9_007_199_254_740_992_i64
        });
        let violations = check_int53_bounds(&overflow_json, "root");
        assert_eq!(violations.len(), 2);
        assert!(violations[0].message.contains("exceeds JS safe integer"));
    }

    #[test]
    fn test_casing_divergence_detection() {
        assert!(CasingStyle::CamelCase.matches("userId"));
        assert!(!CasingStyle::CamelCase.matches("user_id"));
        assert!(!CasingStyle::CamelCase.matches("UserId"));

        assert!(CasingStyle::SnakeCase.matches("user_id"));
        assert!(!CasingStyle::SnakeCase.matches("userId"));

        let payload = serde_json::json!({
            "validCamel": 1,
            "bad_snake": 2
        });
        let violations = check_casing(&payload, CasingStyle::CamelCase, "root");
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].path, "root.bad_snake");
    }

    #[test]
    fn test_null_slice_divergence_detection() {
        let ref_json = serde_json::json!({
            "items": []
        });
        let cand_json = serde_json::json!({
            "items": null
        });
        let violations = check_null_slice_divergence(&ref_json, &cand_json, "root");
        assert_eq!(violations.len(), 1);
        assert!(matches!(
            violations[0].kind,
            ContractViolationKind::NullSliceDivergence { .. }
        ));
    }

    #[test]
    fn test_cross_boundary_contract_fuzz_success() {
        let options = CrossBoundaryOptions {
            diff_options: DiffFuzzOptions {
                iterations: 20,
                ..Default::default()
            },
            expected_casing: Some(CasingStyle::CamelCase),
        };
        let res = run_cross_boundary_diff_fuzz(
            &options,
            |i| i,
            |&i| serde_json::json!({ "id": i, "items": [] }),
            |&i| serde_json::json!({ "id": i, "items": [] }),
        );
        assert!(res.is_ok());
    }

    #[test]
    fn test_cross_boundary_contract_fuzz_failure() {
        let options = CrossBoundaryOptions {
            diff_options: DiffFuzzOptions {
                iterations: 10,
                ..Default::default()
            },
            expected_casing: Some(CasingStyle::CamelCase),
        };
        let res = run_cross_boundary_diff_fuzz(
            &options,
            |i| i,
            |&i| serde_json::json!({ "userId": i, "tags": [] }),
            |&i| serde_json::json!({ "userId": i, "tags": null }),
        );
        assert!(res.is_err());
        let failure = res.unwrap_err();
        assert_eq!(failure.violations.len(), 1);
        assert!(matches!(
            failure.violations[0].kind,
            ContractViolationKind::NullSliceDivergence { .. }
        ));
    }

    #[test]
    fn test_export_reproducer_tests() {
        let spec = ReproducerSpec {
            language: ReproducerLanguage::Rust,
            test_name: "test_divergence",
            input_json: r#"{"id": 42}"#,
            expected_json: Some(r#"{"val": 100}"#),
            candidate_json: Some(r#"{"val": 999}"#),
        };
        let rust_code = export_reproducer_test(&spec);
        assert!(rust_code.contains("mod reproducer_test_divergence"));
        assert!(rust_code.contains("assert_eq!(actual, expected"));

        let ts_spec = ReproducerSpec {
            language: ReproducerLanguage::TypeScript,
            ..spec
        };
        let ts_code = export_reproducer_test(&ts_spec);
        assert!(ts_code.contains("describe(\"Reproducer: test_divergence\""));
        assert!(ts_code.contains("expect(actual).toEqual(expected)"));

        let go_spec = ReproducerSpec {
            language: ReproducerLanguage::Go,
            ..spec
        };
        let go_code = export_reproducer_test(&go_spec);
        assert!(go_code.contains("func TestReproducer_test_divergence"));
        assert!(go_code.contains("reflect.DeepEqual(actual, expected)"));
    }

    #[test]
    fn test_export_reproducer_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repro.rs");
        let spec = ReproducerSpec {
            language: ReproducerLanguage::Rust,
            test_name: "file_test",
            input_json: "{}",
            expected_json: None,
            candidate_json: None,
        };
        assert!(export_reproducer_to_file(&path, &spec).is_ok());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("test_file_test"));
    }
}
