//! Pillar 4: Metamorphic Invariant Testing.
//!
//! Provides metamorphic property test runners (round-trip, idempotence, permutation invariance,
//! state bounds) and multi-language property test scaffolding generators for Rust (`proptest`),
//! TypeScript (`fast-check`), and Go (`pgregory.net/rapid`).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Target programming languages supported for property test suite scaffolding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PropertyLanguage {
    /// Rust property tests powered by `proptest`.
    Rust,
    /// TypeScript / JavaScript property tests powered by `fast-check`.
    TypeScript,
    /// Go property tests powered by `pgregory.net/rapid`.
    Go,
}

impl FromStr for PropertyLanguage {
    type Err = InvariantError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "rust" | "rs" => Ok(Self::Rust),
            "typescript" | "ts" | "tsx" | "js" | "javascript" => Ok(Self::TypeScript),
            "go" | "golang" => Ok(Self::Go),
            other => Err(InvariantError::UnsupportedLanguage(other.to_string())),
        }
    }
}

impl fmt::Display for PropertyLanguage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rust => write!(f, "rust"),
            Self::TypeScript => write!(f, "typescript"),
            Self::Go => write!(f, "go"),
        }
    }
}

/// Metamorphic property categories for invariant testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MetamorphicPropertyKind {
    /// Round-trip inversion: decode(encode(x)) == x.
    RoundTrip,
    /// Idempotence: f(f(x)) == f(x).
    Idempotence,
    /// Permutation invariance: f(permute(x)) == f(x).
    PermutationInvariance,
    /// State invariant bounds: P(S_t) holds across all sequential steps.
    StateBounds,
}

impl fmt::Display for MetamorphicPropertyKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RoundTrip => write!(f, "round_trip"),
            Self::Idempotence => write!(f, "idempotence"),
            Self::PermutationInvariance => write!(f, "permutation_invariance"),
            Self::StateBounds => write!(f, "state_bounds"),
        }
    }
}

/// Strongly typed errors produced during invariant testing or suite generation.
#[derive(thiserror::Error, Debug)]
pub enum InvariantError {
    /// Metamorphic property invariant violated by a concrete counterexample.
    #[error("metamorphic invariant violation: {0}")]
    Violation(InvariantViolation),
    /// Unrecognized language requested for scaffolding generation.
    #[error("unsupported property testing language '{0}'. Supported: 'rust', 'typescript', 'go'")]
    UnsupportedLanguage(String),
    /// Underlying I/O error during scaffold export.
    #[error("I/O error during invariant operation: {0}")]
    Io(#[from] std::io::Error),
}

/// Invariant violation details including property kind and failing input repr.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantViolation {
    /// Violated metamorphic property kind.
    pub property: MetamorphicPropertyKind,
    /// Description of the divergence or assertion failure.
    pub details: String,
    /// Debug string representation of the counterexample input.
    pub counterexample_repr: String,
}

impl fmt::Display for InvariantViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} (counterexample: {})",
            self.property, self.details, self.counterexample_repr
        )
    }
}

/// Summary report of metamorphic property testing runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantReport {
    /// Number of property tests evaluated.
    pub total_evaluated: usize,
    /// Number of property tests that passed without violations.
    pub passed: usize,
    /// Discovered invariant violations with counterexamples.
    pub violations: Vec<InvariantViolation>,
}

impl InvariantReport {
    /// Returns true if all evaluated properties passed with zero violations.
    pub fn is_success(&self) -> bool {
        self.violations.is_empty() && self.total_evaluated > 0
    }
}

/// Verifies the round-trip metamorphic invariant: `decode(encode(x)) == x`.
pub fn check_round_trip<T, U, F, G>(
    input: &T,
    encode: F,
    decode: G,
) -> Result<(), InvariantViolation>
where
    T: PartialEq + core::fmt::Debug + Clone,
    F: Fn(&T) -> U,
    G: Fn(&U) -> T,
{
    let encoded = encode(input);
    let decoded = decode(&encoded);

    if decoded != *input {
        return Err(InvariantViolation {
            property: MetamorphicPropertyKind::RoundTrip,
            details: format!(
                "round-trip divergence: original={:?}, decoded={:?}",
                input, decoded
            ),
            counterexample_repr: format!("{:?}", input),
        });
    }

    Ok(())
}

/// Verifies the idempotence metamorphic invariant: `f(f(x)) == f(x)`.
pub fn check_idempotence<T, F>(input: &T, op: F) -> Result<(), InvariantViolation>
where
    T: PartialEq + core::fmt::Debug + Clone,
    F: Fn(&T) -> T,
{
    let once = op(input);
    let twice = op(&once);

    if once != twice {
        return Err(InvariantViolation {
            property: MetamorphicPropertyKind::Idempotence,
            details: format!(
                "idempotence divergence: f(x)={:?}, f(f(x))={:?}",
                once, twice
            ),
            counterexample_repr: format!("{:?}", input),
        });
    }

    Ok(())
}

/// Verifies the permutation invariance property: `f(permute(x)) == f(x)`.
pub fn check_permutation_invariance<T, U, F>(input: &[T], op: F) -> Result<(), InvariantViolation>
where
    T: Clone,
    U: PartialEq + core::fmt::Debug,
    F: Fn(&[T]) -> U,
{
    let original_output = op(input);

    let mut reversed = input.to_vec();
    reversed.reverse();
    let reversed_output = op(&reversed);

    if original_output != reversed_output {
        return Err(InvariantViolation {
            property: MetamorphicPropertyKind::PermutationInvariance,
            details: format!(
                "permutation divergence: f(original)={:?}, f(reversed)={:?}",
                original_output, reversed_output
            ),
            counterexample_repr: format!("len={}", input.len()),
        });
    }

    if input.len() > 2 {
        let mut rotated = input.to_vec();
        rotated.rotate_left(1);
        let rotated_output = op(&rotated);

        if original_output != rotated_output {
            return Err(InvariantViolation {
                property: MetamorphicPropertyKind::PermutationInvariance,
                details: format!(
                    "permutation divergence: f(original)={:?}, f(rotated)={:?}",
                    original_output, rotated_output
                ),
                counterexample_repr: format!("len={}", input.len()),
            });
        }
    }

    Ok(())
}

/// Verifies that state bounds and predicates hold across all sequential transitions.
pub fn check_state_bounds<S, StepFn, PredFn>(
    initial: &S,
    steps: &[StepFn],
    predicate: PredFn,
) -> Result<(), InvariantViolation>
where
    S: Clone + core::fmt::Debug,
    StepFn: Fn(&S) -> S,
    PredFn: Fn(&S) -> bool,
{
    if !predicate(initial) {
        return Err(InvariantViolation {
            property: MetamorphicPropertyKind::StateBounds,
            details: format!("initial state violates predicate: {:?}", initial),
            counterexample_repr: format!("{:?}", initial),
        });
    }

    let mut current = initial.clone();
    for (idx, step) in steps.iter().enumerate() {
        current = step(&current);
        if !predicate(&current) {
            return Err(InvariantViolation {
                property: MetamorphicPropertyKind::StateBounds,
                details: format!("state invariant breached at step {}: {:?}", idx, current),
                counterexample_repr: format!("step={}: {:?}", idx, current),
            });
        }
    }

    Ok(())
}

/// Generates a complete multi-language property test suite scaffold for a symbol.
pub fn scaffold_property_suite(
    symbol: &str,
    lang: PropertyLanguage,
    properties: &[MetamorphicPropertyKind],
) -> String {
    let clean_symbol = symbol.trim();
    let props = if properties.is_empty() {
        &[
            MetamorphicPropertyKind::RoundTrip,
            MetamorphicPropertyKind::Idempotence,
            MetamorphicPropertyKind::PermutationInvariance,
            MetamorphicPropertyKind::StateBounds,
        ][..]
    } else {
        properties
    };

    match lang {
        PropertyLanguage::Rust => scaffold_rust_proptest(clean_symbol, props),
        PropertyLanguage::TypeScript => scaffold_typescript_fastcheck(clean_symbol, props),
        PropertyLanguage::Go => scaffold_go_rapid(clean_symbol, props),
    }
}

fn scaffold_rust_proptest(symbol: &str, properties: &[MetamorphicPropertyKind]) -> String {
    let mut out = String::new();
    out.push_str("//! Property test suite for ");
    out.push_str(symbol);
    out.push_str(" via proptest.\n\n");
    out.push_str("use proptest::prelude::*;\n\n");
    out.push_str("proptest! {\n");

    for prop in properties {
        match prop {
            MetamorphicPropertyKind::RoundTrip => {
                out.push_str(&format!(
                    "    #[test]\n    fn test_{}_round_trip(val in any::<String>()) {{\n",
                    to_snake_case(symbol)
                ));
                out.push_str(&format!(
                    "        let encoded = {symbol}::encode(&val);\n        let decoded = {symbol}::decode(&encoded).expect(\"decode must succeed\");\n        prop_assert_eq!(decoded, val);\n"
                ));
                out.push_str("    }\n\n");
            }
            MetamorphicPropertyKind::Idempotence => {
                out.push_str(&format!(
                    "    #[test]\n    fn test_{}_idempotence(val in any::<String>()) {{\n",
                    to_snake_case(symbol)
                ));
                out.push_str(&format!(
                    "        let once = {symbol}::normalize(&val);\n        let twice = {symbol}::normalize(&once);\n        prop_assert_eq!(twice, once);\n"
                ));
                out.push_str("    }\n\n");
            }
            MetamorphicPropertyKind::PermutationInvariance => {
                out.push_str(&format!(
                    "    #[test]\n    fn test_{}_permutation_invariance(mut items in prop::collection::vec(any::<u64>(), 1..100)) {{\n",
                    to_snake_case(symbol)
                ));
                out.push_str(&format!(
                    "        let baseline = {symbol}::aggregate(&items);\n        items.reverse();\n        let permuted = {symbol}::aggregate(&items);\n        prop_assert_eq!(permuted, baseline);\n"
                ));
                out.push_str("    }\n\n");
            }
            MetamorphicPropertyKind::StateBounds => {
                out.push_str(&format!(
                    "    #[test]\n    fn test_{}_state_bounds(ops in prop::collection::vec(0..10usize, 1..50)) {{\n",
                    to_snake_case(symbol)
                ));
                out.push_str(&format!(
                    "        let mut subject = {symbol}::default();\n        for op in ops {{\n            subject.apply_step(op);\n            prop_assert!(subject.invariants_hold(), \"State invariant violated\");\n        }}\n"
                ));
                out.push_str("    }\n\n");
            }
        }
    }

    out.push_str("}\n");
    out
}

fn scaffold_typescript_fastcheck(symbol: &str, properties: &[MetamorphicPropertyKind]) -> String {
    let mut out = String::new();
    out.push_str("import * as fc from 'fast-check';\n");
    out.push_str("import { describe, it, expect } from 'vitest';\n\n");
    out.push_str(&format!(
        "describe('{} Metamorphic Invariants', () => {{\n",
        symbol
    ));

    for prop in properties {
        match prop {
            MetamorphicPropertyKind::RoundTrip => {
                out.push_str("  it('satisfies round-trip encode/decode equality', () => {\n");
                out.push_str("    fc.assert(\n");
                out.push_str("      fc.property(fc.string(), (input) => {\n");
                out.push_str(&format!(
                    "        const encoded = {symbol}.encode(input);\n        const decoded = {symbol}.decode(encoded);\n        expect(decoded).toEqual(input);\n"
                ));
                out.push_str("      })\n    );\n  });\n\n");
            }
            MetamorphicPropertyKind::Idempotence => {
                out.push_str("  it('satisfies operation idempotence f(f(x)) === f(x)', () => {\n");
                out.push_str("    fc.assert(\n");
                out.push_str("      fc.property(fc.string(), (input) => {\n");
                out.push_str(&format!(
                    "        const once = {symbol}.normalize(input);\n        const twice = {symbol}.normalize(once);\n        expect(twice).toEqual(once);\n"
                ));
                out.push_str("      })\n    );\n  });\n\n");
            }
            MetamorphicPropertyKind::PermutationInvariance => {
                out.push_str(
                    "  it('satisfies permutation invariance under element reordering', () => {\n",
                );
                out.push_str("    fc.assert(\n");
                out.push_str("      fc.property(fc.array(fc.integer()), (items) => {\n");
                out.push_str(&format!(
                    "        const baseline = {symbol}.aggregate(items);\n        const reversed = [...items].reverse();\n        expect({symbol}.aggregate(reversed)).toEqual(baseline);\n"
                ));
                out.push_str("      })\n    );\n  });\n\n");
            }
            MetamorphicPropertyKind::StateBounds => {
                out.push_str(
                    "  it('preserves state bounds across random operation sequences', () => {\n",
                );
                out.push_str("    fc.assert(\n");
                out.push_str("      fc.property(fc.array(fc.nat(100), { minLength: 1, maxLength: 50 }), (ops) => {\n");
                out.push_str(&format!(
                    "        const subject = new {symbol}();\n        for (const op of ops) {{\n          subject.apply(op);\n          expect(subject.invariantHolds()).toBe(true);\n        }}\n"
                ));
                out.push_str("      })\n    );\n  });\n\n");
            }
        }
    }

    out.push_str("});\n");
    out
}

fn scaffold_go_rapid(symbol: &str, properties: &[MetamorphicPropertyKind]) -> String {
    let mut out = String::new();
    out.push_str("package invariants_test\n\n");
    out.push_str("import (\n");
    out.push_str("\t\"testing\"\n\n");
    out.push_str("\t\"pgregory.net/rapid\"\n");
    out.push_str(")\n\n");

    for prop in properties {
        match prop {
            MetamorphicPropertyKind::RoundTrip => {
                out.push_str(&format!(
                    "func Test{}_RoundTrip(t *testing.T) {{\n",
                    to_pascal_case(symbol)
                ));
                out.push_str("\trapid.Check(t, func(t *rapid.T) {\n");
                out.push_str("\t\tinput := rapid.String().Draw(t, \"input\")\n");
                out.push_str(&format!(
                    "\t\tencoded := {symbol}Encode(input)\n\t\tdecoded, err := {symbol}Decode(encoded)\n\t\tif err != nil || decoded != input {{\n\t\t\tt.Fatalf(\"round-trip failed: got %v, want %v\", decoded, input)\n\t\t}}\n"
                ));
                out.push_str("\t})\n}\n\n");
            }
            MetamorphicPropertyKind::Idempotence => {
                out.push_str(&format!(
                    "func Test{}_Idempotence(t *testing.T) {{\n",
                    to_pascal_case(symbol)
                ));
                out.push_str("\trapid.Check(t, func(t *rapid.T) {\n");
                out.push_str("\t\tinput := rapid.String().Draw(t, \"input\")\n");
                out.push_str(&format!(
                    "\t\tonce := {symbol}Normalize(input)\n\t\ttwice := {symbol}Normalize(once)\n\t\tif once != twice {{\n\t\t\tt.Fatalf(\"idempotence breached: once=%v twice=%v\", once, twice)\n\t\t}}\n"
                ));
                out.push_str("\t})\n}\n\n");
            }
            MetamorphicPropertyKind::PermutationInvariance => {
                out.push_str(&format!(
                    "func Test{}_PermutationInvariance(t *testing.T) {{\n",
                    to_pascal_case(symbol)
                ));
                out.push_str("\trapid.Check(t, func(t *rapid.T) {\n");
                out.push_str("\t\titems := rapid.SliceOf(rapid.Int()).Draw(t, \"items\")\n");
                out.push_str(&format!(
                    "\t\tbase := {symbol}Aggregate(items)\n\t\treversed := make([]int, len(items))\n\t\tfor i, v := range items {{\n\t\t\treversed[len(items)-1-i] = v\n\t\t}}\n\t\tperm := {symbol}Aggregate(reversed)\n\t\tif base != perm {{\n\t\t\tt.Fatalf(\"permutation divergence: base=%v, perm=%v\", base, perm)\n\t\t}}\n"
                ));
                out.push_str("\t})\n}\n\n");
            }
            MetamorphicPropertyKind::StateBounds => {
                out.push_str(&format!(
                    "func Test{}_StateBounds(t *testing.T) {{\n",
                    to_pascal_case(symbol)
                ));
                out.push_str("\trapid.Check(t, func(t *rapid.T) {\n");
                out.push_str("\t\tops := rapid.SliceOf(rapid.IntRange(0, 100)).Draw(t, \"ops\")\n");
                out.push_str(&format!(
                    "\t\tsm := New{symbol}()\n\t\tfor _, op := range ops {{\n\t\t\tsm.Apply(op)\n\t\t\tif !sm.InvariantHolds() {{\n\t\t\t\tt.Fatalf(\"state bounds breached on op %v\", op)\n\t\t\t}}\n\t\t}}\n"
                ));
                out.push_str("\t})\n}\n\n");
            }
        }
    }

    out
}

fn to_snake_case(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch == '-' || ch == ' ' {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    out
}

fn to_pascal_case(s: &str) -> String {
    let mut out = String::new();
    let mut cap_next = true;
    for ch in s.chars() {
        if ch == '_' || ch == '-' || ch == ' ' {
            cap_next = true;
        } else if cap_next {
            out.push(ch.to_ascii_uppercase());
            cap_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_trip_success_and_failure() {
        let input = "hello world".to_string();
        let encode = |s: &String| s.as_bytes().to_vec();
        let decode = |b: &Vec<u8>| String::from_utf8(b.clone()).unwrap();

        assert!(check_round_trip(&input, encode, decode).is_ok());

        let bad_decode = |_b: &Vec<u8>| "corrupted".to_string();
        let err = check_round_trip(&input, encode, bad_decode).unwrap_err();
        assert_eq!(err.property, MetamorphicPropertyKind::RoundTrip);
    }

    #[test]
    fn test_idempotence_success_and_failure() {
        let input = "  trimmed string   ".to_string();
        let trim_op = |s: &String| s.trim().to_string();
        assert!(check_idempotence(&input, trim_op).is_ok());

        let non_idempotent = |s: &String| format!("{}_suffix", s);
        let err = check_idempotence(&input, non_idempotent).unwrap_err();
        assert_eq!(err.property, MetamorphicPropertyKind::Idempotence);
    }

    #[test]
    fn test_permutation_invariance_success_and_failure() {
        let numbers = vec![1, 2, 3, 4, 5];
        let sum_op = |slice: &[i32]| slice.iter().sum::<i32>();
        assert!(check_permutation_invariance(&numbers, sum_op).is_ok());

        let order_sensitive_op = |slice: &[i32]| {
            slice
                .iter()
                .enumerate()
                .map(|(i, &x)| (i as i32 + 1) * x)
                .sum::<i32>()
        };
        let err = check_permutation_invariance(&numbers, order_sensitive_op).unwrap_err();
        assert_eq!(err.property, MetamorphicPropertyKind::PermutationInvariance);
    }

    #[test]
    fn test_state_bounds_success_and_failure() {
        let initial: i32 = 0;
        let inc_steps: [fn(&i32) -> i32; 3] = [|&x| x + 1, |&x| x + 2, |&x| x + 3];

        let bounds_predicate = |&x: &i32| (0..=10).contains(&x);
        assert!(check_state_bounds(&initial, &inc_steps, bounds_predicate).is_ok());

        let exceeding_steps: [fn(&i32) -> i32; 2] = [
            |&x| x + 5,
            |&x| x + 10, // reaches 15 > 10
        ];
        let err = check_state_bounds(&initial, &exceeding_steps, bounds_predicate).unwrap_err();
        assert_eq!(err.property, MetamorphicPropertyKind::StateBounds);
        assert!(err.details.contains("step 1"));
    }

    #[test]
    fn test_property_language_parsing() {
        assert_eq!(
            PropertyLanguage::from_str("rust").unwrap(),
            PropertyLanguage::Rust
        );
        assert_eq!(
            PropertyLanguage::from_str("rs").unwrap(),
            PropertyLanguage::Rust
        );
        assert_eq!(
            PropertyLanguage::from_str("typescript").unwrap(),
            PropertyLanguage::TypeScript
        );
        assert_eq!(
            PropertyLanguage::from_str("ts").unwrap(),
            PropertyLanguage::TypeScript
        );
        assert_eq!(
            PropertyLanguage::from_str("go").unwrap(),
            PropertyLanguage::Go
        );
        assert_eq!(
            PropertyLanguage::from_str("golang").unwrap(),
            PropertyLanguage::Go
        );
        assert!(PropertyLanguage::from_str("python").is_err());
    }

    #[test]
    fn test_scaffold_rust_proptest() {
        let code = scaffold_property_suite("UserSession", PropertyLanguage::Rust, &[]);
        assert!(code.contains("proptest! {"));
        assert!(code.contains("test_user_session_round_trip"));
        assert!(code.contains("test_user_session_idempotence"));
        assert!(code.contains("test_user_session_permutation_invariance"));
        assert!(code.contains("test_user_session_state_bounds"));
    }

    #[test]
    fn test_scaffold_typescript_fastcheck() {
        let code = scaffold_property_suite("UserSession", PropertyLanguage::TypeScript, &[]);
        assert!(code.contains("import * as fc from 'fast-check';"));
        assert!(code.contains("describe('UserSession Metamorphic Invariants'"));
        assert!(code.contains("fc.property(fc.string()"));
        assert!(code.contains("fc.property(fc.array(fc.integer())"));
    }

    #[test]
    fn test_scaffold_go_rapid() {
        let code = scaffold_property_suite("UserSession", PropertyLanguage::Go, &[]);
        assert!(code.contains("package invariants_test"));
        assert!(code.contains("\"pgregory.net/rapid\""));
        assert!(code.contains("func TestUserSession_RoundTrip(t *testing.T)"));
        assert!(code.contains("func TestUserSession_Idempotence(t *testing.T)"));
        assert!(code.contains("func TestUserSession_PermutationInvariance(t *testing.T)"));
        assert!(code.contains("func TestUserSession_StateBounds(t *testing.T)"));
    }
}
