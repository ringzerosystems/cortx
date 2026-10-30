//! Context bundling and prompt formatting for AI coding agents.

use crate::error::DistillError;
use crate::indexer::{CallEdge, ContractInfo, CrossBoundaryImpact};
use cortx_core::output::OutputFormat;
use serde::{Deserialize, Serialize};

/// Degradation stage selected by the advisory bloat guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BudgetStage {
    /// Full context bundle delivered with call graph.
    Full,
    /// Call graph pruned; skeleton and trait contracts delivered.
    CallGraphPruned,
    /// Advisory context dropped completely (empty payload).
    Dropped,
}

/// Pure stage selection. Invariant: the selected payload is strictly smaller
/// than `raw_len`, or the stage is `Dropped` (empty payload).
#[must_use]
pub const fn decide_stage(full_len: u64, pruned_len: u64, raw_len: u64) -> BudgetStage {
    if raw_len == 0 {
        BudgetStage::Dropped
    } else if full_len < raw_len {
        BudgetStage::Full
    } else if pruned_len < raw_len {
        BudgetStage::CallGraphPruned
    } else {
        BudgetStage::Dropped
    }
}

/// Advisory payload fitted to a read budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetedAdvisory {
    /// Selected degradation stage.
    pub stage: BudgetStage,
    /// Advisory markdown content (empty when `stage == BudgetStage::Dropped`).
    pub markdown: String,
    /// Byte length of the full markdown payload (None when formatting was skipped).
    pub full_len: Option<u64>,
    /// Byte length of the pruned skeleton payload.
    pub pruned_len: u64,
}

/// An assembled bundle of compiler-accurate context for an agent prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DistillBundle {
    /// Target file or symbol inspected.
    pub target: String,
    /// Stripped interface type skeleton.
    pub skeleton: String,
    /// Trait implementations and contracts associated with target types.
    pub contracts: Vec<ContractInfo>,
    /// Upstream callers calling into this module/symbol.
    pub callers: Vec<CallEdge>,
    /// Downstream callees invoked by this module/symbol.
    pub callees: Vec<CallEdge>,
    /// Estimated context token reduction percentage compared to raw source.
    pub token_reduction_pct: f64,
    /// Cross-boundary contract impact between Rust backend and TypeScript frontend.
    #[serde(default)]
    pub cross_boundary: Option<CrossBoundaryImpact>,
}

impl DistillBundle {
    /// Returns a copy of the distill bundle with upstream callers and downstream dependencies stripped.
    ///
    /// Retains the type skeleton, trait contracts, and cross-boundary impact while zeroing
    /// out call-graph edges (`callers` and `callees`). The skeleton-based `token_reduction_pct`
    /// (raw source length vs skeleton length) is preserved.
    #[must_use]
    pub fn without_call_graph(&self) -> Self {
        Self {
            target: self.target.clone(),
            skeleton: self.skeleton.clone(),
            contracts: self.contracts.clone(),
            callers: Vec::new(),
            callees: Vec::new(),
            token_reduction_pct: self.token_reduction_pct,
            cross_boundary: self.cross_boundary.clone(),
        }
    }

    /// Fits this bundle within an agent's raw read budget, selecting the highest-fidelity
    /// representation that remains strictly smaller than `raw_len`.
    ///
    /// Evaluation follows a two-stage degradation hierarchy:
    /// - Stage 0 (`Full`): full bundle including call graph fits within `raw_len`.
    /// - Stage 1 (`CallGraphPruned`): full bundle exceeds `raw_len`, but the stripped type skeleton
    ///   and trait contracts fit.
    /// - Stage 2 (`Dropped`): even the stripped skeleton exceeds `raw_len` (e.g. micro-reads),
    ///   yielding an empty payload so no advisory bloat is injected.
    #[must_use]
    pub fn fit_to_budget(&self, raw_len: u64) -> BudgetedAdvisory {
        let pruned = self.without_call_graph();
        let pruned_md = pruned.format_markdown();
        let pruned_len = pruned_md.len() as u64;

        if raw_len == 0 {
            return BudgetedAdvisory {
                stage: BudgetStage::Dropped,
                markdown: String::new(),
                full_len: None,
                pruned_len,
            };
        }

        let has_call_graph = !self.callers.is_empty() || !self.callees.is_empty();
        if !has_call_graph {
            let stage = decide_stage(pruned_len, pruned_len, raw_len);
            let markdown = if stage == BudgetStage::Dropped {
                String::new()
            } else {
                pruned_md
            };
            return BudgetedAdvisory {
                stage,
                markdown,
                full_len: Some(pruned_len),
                pruned_len,
            };
        }

        if pruned_len >= raw_len {
            return BudgetedAdvisory {
                stage: BudgetStage::Dropped,
                markdown: String::new(),
                full_len: None,
                pruned_len,
            };
        }

        let full_md = self.format_markdown();
        let full_len = full_md.len() as u64;
        let stage = decide_stage(full_len, pruned_len, raw_len);
        let markdown = match stage {
            BudgetStage::Full => full_md,
            BudgetStage::CallGraphPruned => pruned_md,
            BudgetStage::Dropped => String::new(),
        };

        BudgetedAdvisory {
            stage,
            markdown,
            full_len: Some(full_len),
            pruned_len,
        }
    }

    /// Formats the bundle according to the requested OutputFormat.
    pub fn format_with(&self, format: OutputFormat) -> Result<String, DistillError> {
        match format {
            OutputFormat::Markdown => Ok(self.format_markdown()),
            OutputFormat::Json => serde_json::to_string_pretty(self).map_err(DistillError::from),
            OutputFormat::Text => Ok(self.format_text()),
        }
    }

    /// Emits structured markdown optimized for LLM system prompt context injection.
    pub fn format_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# Cortx Context Distillation: `{}`\n\n",
            self.target
        ));
        out.push_str(&format!(
            "> **Token Reduction:** {:.1}% context saved vs raw source.\n\n",
            self.token_reduction_pct
        ));

        let lang = if self.target.ends_with(".ts")
            || self.target.ends_with(".tsx")
            || self.target.ends_with(".js")
            || self.target.ends_with(".jsx")
        {
            "typescript"
        } else if self.target.ends_with(".go") {
            "go"
        } else {
            "rust"
        };
        out.push_str(&format!(
            "## Type Skeleton & Interface Contracts\n\n```{lang}\n"
        ));
        out.push_str(&self.skeleton);
        out.push_str("\n```\n\n");

        if !self.contracts.is_empty() {
            out.push_str("## Trait Contracts\n\n");
            for c in &self.contracts {
                out.push_str(&format!(
                    "- `impl {} for {}` (in `{}`)\n",
                    c.trait_name, c.target_type, c.file
                ));
                for m in &c.methods {
                    out.push_str(&format!("  - `{}`\n", m));
                }
            }
            out.push('\n');
        }

        if !self.callers.is_empty() {
            out.push_str("## Upstream Callers\n\n");
            for c in &self.callers {
                out.push_str(&format!(
                    "- `{}` calls `{}` (in `{}`)\n",
                    c.caller, c.callee, c.file
                ));
            }
            out.push('\n');
        }

        if !self.callees.is_empty() {
            out.push_str("## Downstream Dependencies\n\n");
            for c in &self.callees {
                out.push_str(&format!("- `{}` calls `{}`\n", c.caller, c.callee));
            }
            out.push('\n');
        }

        if let Some(ref cb) = self.cross_boundary {
            out.push_str("## Cross-Boundary Impact\n\n");
            if let Some(ref go_def) = cb.go_definition {
                out.push_str(&format!("- **Go Backend Model:** `{}`\n", go_def));
            }
            if let Some(ref rust_def) = cb.rust_definition {
                out.push_str(&format!("- **Rust Backend Definition:** `{}`\n", rust_def));
            }
            if let Some(ref ts_def) = cb.ts_interface {
                out.push_str(&format!(
                    "- **TypeScript Consumer Interface:** `{}`\n",
                    ts_def
                ));
            }
            if !cb.api_call_sites.is_empty() {
                out.push_str("- **Frontend API Call Sites:**\n");
                for site in &cb.api_call_sites {
                    out.push_str(&format!("  - `{}`\n", site));
                }
            }
            out.push('\n');
        }

        out
    }

    /// Emits plain-text representation.
    pub fn format_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "=== Cortx Context Distillation: {} (Reduction: {:.1}%) ===\n\n",
            self.target, self.token_reduction_pct
        ));
        out.push_str("--- Skeleton ---\n");
        out.push_str(&self.skeleton);
        out.push_str("\n\n");

        if !self.contracts.is_empty() {
            out.push_str("--- Trait Contracts ---\n");
            for c in &self.contracts {
                out.push_str(&format!("impl {} for {}\n", c.trait_name, c.target_type));
            }
            out.push('\n');
        }

        if !self.callers.is_empty() {
            out.push_str("--- Callers ---\n");
            for c in &self.callers {
                out.push_str(&format!("{} -> {}\n", c.caller, c.callee));
            }
            out.push('\n');
        }

        if let Some(ref cb) = self.cross_boundary {
            out.push_str("--- Cross-Boundary Impact ---\n");
            if let Some(ref go_def) = cb.go_definition {
                out.push_str(&format!("Go Backend Model: {}\n", go_def));
            }
            if let Some(ref rust_def) = cb.rust_definition {
                out.push_str(&format!("Rust Backend Definition: {}\n", rust_def));
            }
            if let Some(ref ts_def) = cb.ts_interface {
                out.push_str(&format!("TypeScript Consumer Interface: {}\n", ts_def));
            }
            for site in &cb.api_call_sites {
                out.push_str(&format!("Call Site: {}\n", site));
            }
            out.push('\n');
        }

        out
    }
}

#[cfg(kani)]
#[kani::proof]
fn decide_stage_never_exceeds_budget() {
    let full_len: u64 = kani::any();
    let pruned_len: u64 = kani::any();
    let raw_len: u64 = kani::any();
    kani::assume(pruned_len <= full_len);

    let stage = decide_stage(full_len, pruned_len, raw_len);
    match stage {
        BudgetStage::Full => assert!(full_len < raw_len),
        BudgetStage::CallGraphPruned => {
            assert!(pruned_len < raw_len);
            assert!(full_len >= raw_len);
        }
        BudgetStage::Dropped => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::{CallEdge, ContractInfo, CrossBoundaryImpact};

    #[test]
    fn test_without_call_graph_prunes_edges_and_preserves_skeleton() {
        let bundle = DistillBundle {
            target: "src/service.rs".to_string(),
            skeleton: "pub struct UserService;\nimpl UserService { pub fn find_user(&self); }"
                .to_string(),
            contracts: vec![ContractInfo {
                trait_name: "Service".to_string(),
                target_type: "UserService".to_string(),
                methods: vec!["find_user".to_string()],
                file: "src/service.rs".to_string(),
            }],
            callers: vec![CallEdge {
                caller: "handle_request".to_string(),
                callee: "UserService::find_user".to_string(),
                file: "src/handler.rs".to_string(),
            }],
            callees: vec![CallEdge {
                caller: "UserService::find_user".to_string(),
                callee: "db_query".to_string(),
                file: "src/service.rs".to_string(),
            }],
            token_reduction_pct: 72.5,
            cross_boundary: Some(CrossBoundaryImpact {
                symbol: "UserService".to_string(),
                rust_definition: Some("src/service.rs:10".to_string()),
                ts_interface: Some("frontend/src/api.ts:20".to_string()),
                go_definition: None,
                api_call_sites: vec!["fetch('/api/users')".to_string()],
            }),
        };

        let full_md = bundle.format_markdown();
        assert!(full_md.contains("## Upstream Callers"));
        assert!(full_md.contains("## Downstream Dependencies"));
        assert!(full_md.contains("## Type Skeleton & Interface Contracts"));
        assert!(full_md.contains("## Trait Contracts"));
        assert!(full_md.contains("## Cross-Boundary Impact"));

        let pruned = bundle.without_call_graph();
        assert_eq!(pruned.target, bundle.target);
        assert_eq!(pruned.skeleton, bundle.skeleton);
        assert_eq!(pruned.contracts, bundle.contracts);
        assert!(pruned.callers.is_empty());
        assert!(pruned.callees.is_empty());
        assert_eq!(pruned.token_reduction_pct, bundle.token_reduction_pct);
        assert_eq!(pruned.cross_boundary, bundle.cross_boundary);

        let pruned_md = pruned.format_markdown();
        assert!(!pruned_md.contains("## Upstream Callers"));
        assert!(!pruned_md.contains("## Downstream Dependencies"));
        assert!(pruned_md.contains("## Type Skeleton & Interface Contracts"));
        assert!(pruned_md.contains("## Trait Contracts"));
        assert!(pruned_md.contains("## Cross-Boundary Impact"));

        let pruned_text = pruned.format_text();
        assert!(!pruned_text.contains("--- Callers ---"));
        assert!(pruned_text.contains("--- Skeleton ---"));
        assert!(pruned_text.contains("--- Trait Contracts ---"));
        assert!(pruned_text.contains("--- Cross-Boundary Impact ---"));
    }

    #[test]
    fn test_decide_stage_branches() {
        assert_eq!(decide_stage(100, 50, 0), BudgetStage::Dropped);
        assert_eq!(decide_stage(100, 50, 200), BudgetStage::Full);
        assert_eq!(decide_stage(100, 50, 80), BudgetStage::CallGraphPruned);
        assert_eq!(decide_stage(100, 50, 50), BudgetStage::Dropped);
        assert_eq!(decide_stage(100, 50, 30), BudgetStage::Dropped);
    }

    #[test]
    fn test_fit_to_budget_lazy_formatting_and_zero_budget() {
        let bundle = DistillBundle {
            target: "src/service.rs".to_string(),
            skeleton: "pub struct S;".to_string(),
            contracts: vec![],
            callers: vec![CallEdge {
                caller: "caller_fn".into(),
                callee: "S".into(),
                file: "src/caller.rs".into(),
            }],
            callees: vec![],
            token_reduction_pct: 50.0,
            cross_boundary: None,
        };

        // Zero budget: immediate drop, full_len is None
        let b0 = bundle.fit_to_budget(0);
        assert_eq!(b0.stage, BudgetStage::Dropped);
        assert!(b0.markdown.is_empty());
        assert_eq!(b0.full_len, None);

        // Micro-read smaller than pruned skeleton: full markdown never formatted
        let pruned_len = bundle.without_call_graph().format_markdown().len() as u64;
        let b_drop = bundle.fit_to_budget(pruned_len - 1);
        assert_eq!(b_drop.stage, BudgetStage::Dropped);
        assert!(b_drop.markdown.is_empty());
        assert_eq!(b_drop.full_len, None);

        // Bundle without call graph: full_len == Some(pruned_len) without double formatting
        let no_cg = bundle.without_call_graph();
        let b_no_cg = no_cg.fit_to_budget(pruned_len + 10);
        assert_eq!(b_no_cg.stage, BudgetStage::Full);
        assert_eq!(b_no_cg.full_len, Some(pruned_len));
    }

    #[test]
    fn test_fit_to_budget_ordering_large_callgraph_skip_full_formatting() {
        let callers: Vec<CallEdge> = (0..5000)
            .map(|i| CallEdge {
                caller: format!("caller_{i}"),
                callee: "S".into(),
                file: format!("src/caller_{i}.rs"),
            })
            .collect();
        let bundle = DistillBundle {
            target: "src/service.rs".to_string(),
            skeleton: "pub struct S;".to_string(),
            contracts: vec![],
            callers,
            callees: vec![],
            token_reduction_pct: 50.0,
            cross_boundary: None,
        };

        let pruned_len = bundle.without_call_graph().format_markdown().len() as u64;

        // A bundle with 5,000 caller edges and raw_len smaller than the pruned skeleton -> full_len == None
        let b_oversized = bundle.fit_to_budget(pruned_len - 1);
        assert_eq!(b_oversized.stage, BudgetStage::Dropped);
        assert!(b_oversized.markdown.is_empty());
        assert_eq!(b_oversized.full_len, None);

        // A bundle with no call graph -> full_len == Some(pruned_len)
        let no_cg = bundle.without_call_graph();
        let b_no_cg = no_cg.fit_to_budget(pruned_len + 50);
        assert_eq!(b_no_cg.stage, BudgetStage::Full);
        assert_eq!(b_no_cg.full_len, Some(pruned_len));
    }

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]
        #[test]
        fn proptest_decide_stage_properties(
            (pruned_len, full_len) in (0u64..100_000).prop_flat_map(|p| (Just(p), p..100_000)),
            raw_len in 0u64..100_000,
            extra_raw in 0u64..100_000,
        ) {
            let stage = decide_stage(full_len, pruned_len, raw_len);
            if raw_len == 0 {
                prop_assert_eq!(stage, BudgetStage::Dropped);
            }
            match stage {
                BudgetStage::Full => {
                    prop_assert!(full_len < raw_len);
                }
                BudgetStage::CallGraphPruned => {
                    prop_assert!(pruned_len < raw_len);
                    prop_assert!(full_len >= raw_len);
                }
                BudgetStage::Dropped => {
                    prop_assert!(raw_len == 0 || pruned_len >= raw_len);
                }
            }

            // Monotonicity: increasing raw_len never moves stage backwards
            let higher_raw = raw_len.saturating_add(extra_raw);
            let higher_stage = decide_stage(full_len, pruned_len, higher_raw);
            let stage_rank = |s: BudgetStage| match s {
                BudgetStage::Dropped => 0,
                BudgetStage::CallGraphPruned => 1,
                BudgetStage::Full => 2,
            };
            prop_assert!(stage_rank(higher_stage) >= stage_rank(stage));
        }

        #[test]
        fn proptest_fit_to_budget_bounds(
            skeleton in "[a-zA-Z0-9_\n\t ]{0,300}",
            caller_count in 0usize..20,
            callee_count in 0usize..20,
            raw_len in 0u64..5_000,
        ) {
            let callers = (0..caller_count)
                .map(|i| CallEdge {
                    caller: format!("caller_{i}"),
                    callee: "fn_callee".into(),
                    file: "caller.rs".into(),
                })
                .collect();
            let callees = (0..callee_count)
                .map(|i| CallEdge {
                    caller: "fn_callee".into(),
                    callee: format!("callee_{i}"),
                    file: "callee.rs".into(),
                })
                .collect();

            let bundle = DistillBundle {
                target: "src/lib.rs".to_string(),
                skeleton,
                contracts: Vec::new(),
                callers,
                callees,
                token_reduction_pct: 50.0,
                cross_boundary: None,
            };

            let budgeted = bundle.fit_to_budget(raw_len);
            if budgeted.stage == BudgetStage::Dropped {
                prop_assert!(budgeted.markdown.is_empty());
            } else {
                prop_assert!((budgeted.markdown.len() as u64) < raw_len);
            }

            if let Some(full) = budgeted.full_len {
                prop_assert_eq!(budgeted.stage, decide_stage(full, budgeted.pruned_len, raw_len));
            }
        }
    }
}
