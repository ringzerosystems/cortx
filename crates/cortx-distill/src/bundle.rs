//! Context bundling and prompt formatting for AI coding agents.

use crate::error::DistillError;
use crate::indexer::{CallEdge, ContractInfo, CrossBoundaryImpact};
use cortx_core::ReadCoverage;
use cortx_core::output::OutputFormat;
use serde::{Deserialize, Serialize};

/// Maximum wire character limit for hook post-read advisory context payloads (reserving 800 chars under 10k).
pub const MAX_HOOK_WIRE_CHARS: usize = 9_200;

/// Disposition of an advisory payload packed for host post-read delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdvisoryDisposition {
    /// Omitted completely due to micro-read window (<= 150 lines).
    OmittedMicro,
    /// Delivered for full-file read with target type skeleton omitted.
    FullReadNoSkeleton,
    /// Delivered for partial macro read with unread outline and proximity ranking.
    PartialMacroDelivered,
    /// Tail-trimmed to stay strictly within the 9,200 character wire budget.
    Trimmed,
}

/// Advisory payload packed specifically for tool hook execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackedAdvisory {
    /// Delivered markdown content.
    pub markdown: String,
    /// Exact count of Unicode characters in `markdown`.
    pub wire_chars: usize,
    /// Disposition indicating tier and trimming state.
    pub disposition: AdvisoryDisposition,
}

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
    /// Compact outline of unread declarations outside the read window (PartialMacro).
    #[serde(default)]
    pub unread_outline: Option<String>,
    /// Number of upstream callers omitted due to per-section or budget ceilings.
    #[serde(default)]
    pub omitted_callers: usize,
    /// Number of downstream callees omitted due to per-section or budget ceilings.
    #[serde(default)]
    pub omitted_callees: usize,
}

impl DistillBundle {
    /// Returns true if all packed sections (skeleton, contracts, callers, callees, cross-boundary, outline) are empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.skeleton.is_empty()
            && self.contracts.is_empty()
            && self.callers.is_empty()
            && self.callees.is_empty()
            && self.cross_boundary.is_none()
            && self.unread_outline.as_deref().unwrap_or("").is_empty()
    }

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
            unread_outline: self.unread_outline.clone(),
            omitted_callers: 0,
            omitted_callees: 0,
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
        if self.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        out.push_str(&format!(
            "# Cortx Context Distillation: `{}`\n\n",
            self.target
        ));

        let lang = target_lang(&self.target);
        if !self.skeleton.is_empty() {
            out.push_str(&format!(
                "## Type Skeleton & Interface Contracts\n\n```{lang}\n"
            ));
            out.push_str(&self.skeleton);
            out.push_str("\n```\n\n");
        }

        if !self.contracts.is_empty() {
            out.push_str(&format_contracts(&self.contracts, 0));
        }

        if !self.callers.is_empty() {
            out.push_str(&format_callers(&self.callers, self.omitted_callers));
        }

        if !self.callees.is_empty() {
            out.push_str(&format_callees(&self.callees, self.omitted_callees));
        }

        if let Some(ref cb) = self.cross_boundary {
            out.push_str(&format_cross_boundary(cb, None));
        }

        if let Some(ref outline) = self.unread_outline {
            let lines: Vec<&str> = outline.lines().filter(|l| !l.trim().is_empty()).collect();
            out.push_str(&format_outline(&lines, lang));
        }

        out
    }

    /// Emits plain-text representation.
    pub fn format_text(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        out.push_str(&format!(
            "=== Cortx Context Distillation: {} ===\n\n",
            self.target
        ));
        if !self.skeleton.is_empty() {
            out.push_str("--- Skeleton ---\n");
            out.push_str(&self.skeleton);
            out.push_str("\n\n");
        }

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
                if !c.file.is_empty() {
                    if c.call_line > 0 {
                        out.push_str(&format!(
                            "{} -> {} (in {}:{})\n",
                            c.caller, c.callee, c.file, c.call_line
                        ));
                    } else {
                        out.push_str(&format!("{} -> {} (in {})\n", c.caller, c.callee, c.file));
                    }
                } else {
                    out.push_str(&format!("{} -> {}\n", c.caller, c.callee));
                }
            }
            if self.omitted_callers > 0 {
                out.push_str(&format!("... +{} callers omitted\n", self.omitted_callers));
            }
            out.push('\n');
        }

        if !self.callees.is_empty() {
            out.push_str("--- Downstream Dependencies ---\n");
            for c in &self.callees {
                if !c.file.is_empty() {
                    if c.call_line > 0 {
                        out.push_str(&format!(
                            "{} -> {} (in {}:{})\n",
                            c.caller, c.callee, c.file, c.call_line
                        ));
                    } else {
                        out.push_str(&format!("{} -> {} (in {})\n", c.caller, c.callee, c.file));
                    }
                } else {
                    out.push_str(&format!("{} -> {}\n", c.caller, c.callee));
                }
            }
            if self.omitted_callees > 0 {
                out.push_str(&format!("... +{} callees omitted\n", self.omitted_callees));
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

        if let Some(ref outline) = self.unread_outline {
            if !outline.trim().is_empty() {
                out.push_str("--- Unread Outline ---\n");
                out.push_str(outline);
                if !outline.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n');
            }
        }

        out
    }

    /// Packs an advisory context payload specifically tailored for post-read delivery,
    /// enforcing character limits (`<= 9_200` chars), omitting target-file skeletons on
    /// full-file reads, and embedding unread outlines on partial macro reads.
    pub fn pack_for_hook(&self, coverage: ReadCoverage) -> PackedAdvisory {
        match coverage {
            ReadCoverage::Micro => PackedAdvisory {
                markdown: String::new(),
                wire_chars: 0,
                disposition: AdvisoryDisposition::OmittedMicro,
            },
            ReadCoverage::FullFile => {
                let has_sections = self.cross_boundary.is_some()
                    || !self.callers.is_empty()
                    || !self.callees.is_empty()
                    || !self.contracts.is_empty();

                if !has_sections {
                    return PackedAdvisory {
                        markdown: String::new(),
                        wire_chars: 0,
                        disposition: AdvisoryDisposition::FullReadNoSkeleton,
                    };
                }

                self.pack_macro_sections(false)
            }
            ReadCoverage::PartialMacro => {
                let has_outline = self
                    .unread_outline
                    .as_deref()
                    .is_some_and(|s| !s.trim().is_empty());
                let has_sections = self.cross_boundary.is_some()
                    || !self.callers.is_empty()
                    || !self.callees.is_empty()
                    || !self.contracts.is_empty()
                    || has_outline;

                if !has_sections {
                    return PackedAdvisory {
                        markdown: String::new(),
                        wire_chars: 0,
                        disposition: AdvisoryDisposition::PartialMacroDelivered,
                    };
                }

                self.pack_macro_sections(true)
            }
        }
    }

    fn pack_macro_sections(&self, include_outline: bool) -> PackedAdvisory {
        let default_disposition = if include_outline {
            AdvisoryDisposition::PartialMacroDelivered
        } else {
            AdvisoryDisposition::FullReadNoSkeleton
        };

        let lang = target_lang(&self.target);
        let header = format!("# Cortx Context Distillation: `{}`\n\n", self.target);

        let outline_lines: Vec<&str> = if include_outline {
            self.unread_outline
                .as_deref()
                .unwrap_or("")
                .lines()
                .filter(|l| !l.trim().is_empty())
                .collect()
        } else {
            Vec::new()
        };

        let mut s = AdvisoryState {
            header: &header,
            cb: self.cross_boundary.as_ref(),
            api_calls_limit: None,
            callers: &self.callers,
            num_callers: self.callers.len(),
            base_omitted_callers: self.omitted_callers,
            callees: &self.callees,
            num_callees: self.callees.len(),
            base_omitted_callees: self.omitted_callees,
            contracts: &self.contracts,
            num_contracts: self.contracts.len(),
            outline_lines: &outline_lines,
            num_outline: outline_lines.len(),
            lang,
        };

        let initial_md = render_advisory(&s);
        if initial_md.chars().count() <= MAX_HOOK_WIRE_CHARS {
            let safe_md = ensure_wire_budget_and_fences(initial_md);
            let wire_chars = safe_md.chars().count();
            return PackedAdvisory {
                markdown: safe_md,
                wire_chars,
                disposition: default_disposition,
            };
        }

        // Trimming priority order: outline -> contracts -> callees -> callers -> cross boundary
        while s.num_outline > 0 {
            s.num_outline -= 1;
            let cand = render_advisory(&s);
            if cand.chars().count() <= MAX_HOOK_WIRE_CHARS {
                return packed_trimmed(cand);
            }
        }

        while s.num_contracts > 0 {
            s.num_contracts -= 1;
            let cand = render_advisory(&s);
            if cand.chars().count() <= MAX_HOOK_WIRE_CHARS {
                return packed_trimmed(cand);
            }
        }

        while s.num_callees > 0 {
            s.num_callees -= 1;
            let cand = render_advisory(&s);
            if cand.chars().count() <= MAX_HOOK_WIRE_CHARS {
                return packed_trimmed(cand);
            }
        }

        while s.num_callers > 0 {
            s.num_callers -= 1;
            let cand = render_advisory(&s);
            if cand.chars().count() <= MAX_HOOK_WIRE_CHARS {
                return packed_trimmed(cand);
            }
        }

        if let Some(cb) = s.cb {
            let mut api_limit = cb.api_call_sites.len();
            while api_limit > 0 {
                api_limit -= 1;
                s.api_calls_limit = Some(api_limit);
                let cand = render_advisory(&s);
                if cand.chars().count() <= MAX_HOOK_WIRE_CHARS {
                    return packed_trimmed(cand);
                }
            }
            s.cb = None;
            let cand = render_advisory(&s);
            if cand.chars().count() <= MAX_HOOK_WIRE_CHARS {
                return packed_trimmed(cand);
            }
        }

        let fallback_md = render_advisory(&s);
        packed_trimmed(fallback_md)
    }
}

struct AdvisoryState<'a> {
    header: &'a str,
    cb: Option<&'a CrossBoundaryImpact>,
    api_calls_limit: Option<usize>,
    callers: &'a [CallEdge],
    num_callers: usize,
    base_omitted_callers: usize,
    callees: &'a [CallEdge],
    num_callees: usize,
    base_omitted_callees: usize,
    contracts: &'a [ContractInfo],
    num_contracts: usize,
    outline_lines: &'a [&'a str],
    num_outline: usize,
    lang: &'a str,
}

fn render_advisory(s: &AdvisoryState<'_>) -> String {
    let mut out = String::from(s.header);
    if let Some(cb) = s.cb {
        out.push_str(&format_cross_boundary(cb, s.api_calls_limit));
    }
    let omitted_callers = s.base_omitted_callers + (s.callers.len() - s.num_callers);
    out.push_str(&format_callers(
        &s.callers[..s.num_callers],
        omitted_callers,
    ));

    let omitted_callees = s.base_omitted_callees + (s.callees.len() - s.num_callees);
    out.push_str(&format_callees(
        &s.callees[..s.num_callees],
        omitted_callees,
    ));

    let omitted_contracts = s.contracts.len() - s.num_contracts;
    out.push_str(&format_contracts(
        &s.contracts[..s.num_contracts],
        omitted_contracts,
    ));

    out.push_str(&format_outline(&s.outline_lines[..s.num_outline], s.lang));
    out
}

fn format_callers(callers: &[CallEdge], omitted: usize) -> String {
    if callers.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Upstream Callers\n\n");
    for c in callers {
        if c.file.is_empty() {
            out.push_str(&format!("- `{}` calls `{}`\n", c.caller, c.callee));
        } else if c.call_line > 0 {
            out.push_str(&format!(
                "- `{}` calls `{}` (in `{}:{}`)\n",
                c.caller, c.callee, c.file, c.call_line
            ));
        } else {
            out.push_str(&format!(
                "- `{}` calls `{}` (in `{}`)\n",
                c.caller, c.callee, c.file
            ));
        }
    }
    if omitted > 0 {
        out.push_str(&format!("_... +{} callers omitted_\n", omitted));
    }
    out.push('\n');
    out
}

fn format_callees(callees: &[CallEdge], omitted: usize) -> String {
    if callees.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Downstream Dependencies\n\n");
    for c in callees {
        if c.file.is_empty() {
            out.push_str(&format!("- `{}` calls `{}`\n", c.caller, c.callee));
        } else if c.call_line > 0 {
            out.push_str(&format!(
                "- `{}` calls `{}` (in `{}:{}`)\n",
                c.caller, c.callee, c.file, c.call_line
            ));
        } else {
            out.push_str(&format!(
                "- `{}` calls `{}` (in `{}`)\n",
                c.caller, c.callee, c.file
            ));
        }
    }
    if omitted > 0 {
        out.push_str(&format!("_... +{} callees omitted_\n", omitted));
    }
    out.push('\n');
    out
}

fn format_contracts(contracts: &[ContractInfo], omitted: usize) -> String {
    if contracts.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Trait Contracts\n\n");
    for c in contracts {
        out.push_str(&format!(
            "- `impl {} for {}` (in `{}`)\n",
            c.trait_name, c.target_type, c.file
        ));
        for m in &c.methods {
            out.push_str(&format!("  - `{}`\n", m));
        }
    }
    if omitted > 0 {
        out.push_str(&format!("_... +{} contracts omitted_\n", omitted));
    }
    out.push('\n');
    out
}

fn format_outline(lines: &[&str], lang: &str) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut out = format!("## Unread File Outline\n\n```{lang}\n");
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("```\n\n");
    out
}

fn format_cross_boundary(cb: &CrossBoundaryImpact, api_limit: Option<usize>) -> String {
    let mut out = String::from("## Cross-Boundary Impact\n\n");
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
    let sites = match api_limit {
        Some(limit) => &cb.api_call_sites[..limit.min(cb.api_call_sites.len())],
        None => &cb.api_call_sites[..],
    };
    if !sites.is_empty() {
        out.push_str("- **Frontend API Call Sites:**\n");
        for site in sites {
            out.push_str(&format!("  - `{}`\n", site));
        }
    }
    out.push('\n');
    out
}

fn target_lang(target: &str) -> &'static str {
    if target.ends_with(".ts")
        || target.ends_with(".tsx")
        || target.ends_with(".js")
        || target.ends_with(".jsx")
    {
        "typescript"
    } else if target.ends_with(".go") {
        "go"
    } else {
        "rust"
    }
}

fn packed_trimmed(md: String) -> PackedAdvisory {
    let safe_md = ensure_wire_budget_and_fences(md);
    let wire_chars = safe_md.chars().count();
    PackedAdvisory {
        markdown: safe_md,
        wire_chars,
        disposition: AdvisoryDisposition::Trimmed,
    }
}

fn ensure_wire_budget_and_fences(mut md: String) -> String {
    if md.matches("```").count() % 2 != 0 {
        if md.chars().count() + 5 > MAX_HOOK_WIRE_CHARS {
            let target_chars = MAX_HOOK_WIRE_CHARS.saturating_sub(5);
            let truncated: String = md.chars().take(target_chars).collect();
            md = truncated;
            if md.matches("```").count() % 2 != 0 {
                md.push_str("\n```\n");
            }
        } else {
            md.push_str("\n```\n");
        }
    }
    if md.chars().count() > MAX_HOOK_WIRE_CHARS {
        let truncated: String = md.chars().take(MAX_HOOK_WIRE_CHARS).collect();
        md = truncated;
        if md.matches("```").count() % 2 != 0 {
            let target_chars = MAX_HOOK_WIRE_CHARS.saturating_sub(5);
            let mut safe: String = md.chars().take(target_chars).collect();
            safe.push_str("\n```\n");
            md = safe;
        }
    }
    md
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
                call_line: 0,
            }],
            callees: vec![CallEdge {
                caller: "UserService::find_user".to_string(),
                callee: "db_query".to_string(),
                file: "src/service.rs".to_string(),
                call_line: 0,
            }],
            token_reduction_pct: 72.5,
            cross_boundary: Some(CrossBoundaryImpact {
                symbol: "UserService".to_string(),
                rust_definition: Some("src/service.rs:10".to_string()),
                ts_interface: Some("frontend/src/api.ts:20".to_string()),
                go_definition: None,
                api_call_sites: vec!["fetch('/api/users')".to_string()],
            }),
            unread_outline: None,
            omitted_callers: 0,
            omitted_callees: 0,
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
                call_line: 0,
            }],
            callees: vec![],
            token_reduction_pct: 50.0,
            cross_boundary: None,
            unread_outline: None,
            omitted_callers: 0,
            omitted_callees: 0,
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
                call_line: 0,
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
            unread_outline: None,
            omitted_callers: 0,
            omitted_callees: 0,
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

    #[test]
    fn test_pack_for_hook_micro_omission() {
        let bundle = DistillBundle {
            target: "src/service.rs".to_string(),
            skeleton: "pub struct S;".to_string(),
            contracts: vec![],
            callers: vec![CallEdge {
                caller: "caller_fn".into(),
                callee: "S".into(),
                file: "src/caller.rs".into(),
                call_line: 12,
            }],
            callees: vec![],
            token_reduction_pct: 50.0,
            cross_boundary: None,
            unread_outline: Some("L1: pub fn head();\n".into()),
            omitted_callers: 0,
            omitted_callees: 0,
        };

        let packed = bundle.pack_for_hook(ReadCoverage::Micro);
        assert_eq!(packed.disposition, AdvisoryDisposition::OmittedMicro);
        assert_eq!(packed.markdown, "");
        assert_eq!(packed.wire_chars, 0);
    }

    #[test]
    fn test_pack_for_hook_full_file_omits_skeleton_and_empty_leaf() {
        // 1. Leaf with no call graph, contracts, or cross boundary -> empty markdown
        let leaf_bundle = DistillBundle {
            target: "src/leaf.rs".to_string(),
            skeleton: "pub struct Leaf;".to_string(),
            contracts: vec![],
            callers: vec![],
            callees: vec![],
            token_reduction_pct: 60.0,
            cross_boundary: None,
            unread_outline: None,
            omitted_callers: 0,
            omitted_callees: 0,
        };
        let leaf_packed = leaf_bundle.pack_for_hook(ReadCoverage::FullFile);
        assert_eq!(
            leaf_packed.disposition,
            AdvisoryDisposition::FullReadNoSkeleton
        );
        assert_eq!(leaf_packed.markdown, "");
        assert_eq!(leaf_packed.wire_chars, 0);

        // 2. Full bundle -> delivers callers, callees, contracts, cross boundary without skeleton
        let full_bundle = DistillBundle {
            target: "src/service.rs".to_string(),
            skeleton: "pub struct Service;".to_string(),
            contracts: vec![ContractInfo {
                trait_name: "ServiceTrait".into(),
                target_type: "Service".into(),
                methods: vec!["run".into()],
                file: "src/service.rs".into(),
            }],
            callers: vec![CallEdge {
                caller: "main".into(),
                callee: "Service::run".into(),
                file: "src/main.rs".into(),
                call_line: 42,
            }],
            callees: vec![CallEdge {
                caller: "Service::run".into(),
                callee: "db_query".into(),
                file: "src/db.rs".into(),
                call_line: 105,
            }],
            token_reduction_pct: 75.0,
            cross_boundary: Some(CrossBoundaryImpact {
                symbol: "Service".into(),
                rust_definition: Some("src/service.rs:10".into()),
                ts_interface: Some("frontend/api.ts:25".into()),
                go_definition: None,
                api_call_sites: vec!["fetch('/api/run')".into()],
            }),
            unread_outline: None,
            omitted_callers: 3,
            omitted_callees: 2,
        };
        let packed = full_bundle.pack_for_hook(ReadCoverage::FullFile);
        assert_eq!(packed.disposition, AdvisoryDisposition::FullReadNoSkeleton);
        assert!(!packed.markdown.contains("## Type Skeleton"));
        assert!(!packed.markdown.contains("pub struct Service;"));
        assert!(!packed.markdown.contains("Token Reduction"));
        assert!(packed.markdown.contains("## Cross-Boundary Impact"));
        assert!(packed.markdown.contains("## Upstream Callers"));
        assert!(
            packed
                .markdown
                .contains("- `main` calls `Service::run` (in `src/main.rs:42`)")
        );
        assert!(packed.markdown.contains("_... +3 callers omitted_"));
        assert!(packed.markdown.contains("## Downstream Dependencies"));
        assert!(
            packed
                .markdown
                .contains("- `Service::run` calls `db_query` (in `src/db.rs:105`)")
        );
        assert!(packed.markdown.contains("_... +2 callees omitted_"));
        assert!(packed.markdown.contains("## Trait Contracts"));
        assert!(
            packed
                .markdown
                .contains("- `impl ServiceTrait for Service` (in `src/service.rs`)")
        );
        assert_eq!(packed.wire_chars, packed.markdown.chars().count());
        assert!(packed.wire_chars <= MAX_HOOK_WIRE_CHARS);
    }

    #[test]
    fn test_pack_for_hook_partial_macro_delivers_outline() {
        let bundle = DistillBundle {
            target: "src/service.rs".to_string(),
            skeleton: "pub struct Service;".to_string(),
            contracts: vec![],
            callers: vec![],
            callees: vec![],
            token_reduction_pct: 50.0,
            cross_boundary: None,
            unread_outline: Some("L10: pub fn init();\nL50: pub fn teardown();\n".into()),
            omitted_callers: 0,
            omitted_callees: 0,
        };

        let packed = bundle.pack_for_hook(ReadCoverage::PartialMacro);
        assert_eq!(
            packed.disposition,
            AdvisoryDisposition::PartialMacroDelivered
        );
        assert!(!packed.markdown.contains("## Type Skeleton"));
        assert!(packed.markdown.contains("## Unread File Outline"));
        assert!(packed.markdown.contains("```rust"));
        assert!(packed.markdown.contains("L10: pub fn init();"));
        assert!(packed.markdown.contains("L50: pub fn teardown();"));
        assert_eq!(packed.markdown.matches("```").count(), 2);
    }

    #[test]
    fn test_pack_for_hook_wire_trimming_priority_and_fences() {
        // Create an oversized unread outline (> 10,000 characters)
        let mut big_outline = String::new();
        for i in 0..400 {
            big_outline.push_str(&format!(
                "L{}: pub async fn handler_with_a_very_long_name_for_testing_purposes_{i}(param_a: String, param_b: u64) -> Result<(), DistillError>;\n",
                i + 1
            ));
        }

        let bundle = DistillBundle {
            target: "src/oversized.rs".to_string(),
            skeleton: "pub struct Big;".to_string(),
            contracts: vec![ContractInfo {
                trait_name: "BigTrait".into(),
                target_type: "Big".into(),
                methods: vec!["run".into()],
                file: "src/oversized.rs".into(),
            }],
            callers: vec![CallEdge {
                caller: "caller_1".into(),
                callee: "Big::run".into(),
                file: "src/caller.rs".into(),
                call_line: 10,
            }],
            callees: vec![],
            token_reduction_pct: 40.0,
            cross_boundary: None,
            unread_outline: Some(big_outline),
            omitted_callers: 0,
            omitted_callees: 0,
        };

        let packed = bundle.pack_for_hook(ReadCoverage::PartialMacro);
        assert_eq!(packed.disposition, AdvisoryDisposition::Trimmed);
        assert!(packed.wire_chars <= MAX_HOOK_WIRE_CHARS);
        assert_eq!(packed.wire_chars, packed.markdown.chars().count());

        // Higher-priority sections must be preserved while lower-priority outline lines were trimmed
        assert!(packed.markdown.contains("## Upstream Callers"));
        assert!(packed.markdown.contains("## Trait Contracts"));
        assert_eq!(packed.markdown.matches("```").count() % 2, 0);
    }

    #[test]
    fn test_format_markdown_no_token_reduction_and_formats_lines() {
        let bundle = DistillBundle {
            target: "src/lib.rs".to_string(),
            skeleton: "pub fn hello();".to_string(),
            contracts: vec![],
            callers: vec![CallEdge {
                caller: "test_call".into(),
                callee: "hello".into(),
                file: "tests/test.rs".into(),
                call_line: 99,
            }],
            callees: vec![],
            token_reduction_pct: 88.0,
            cross_boundary: None,
            unread_outline: None,
            omitted_callers: 5,
            omitted_callees: 0,
        };

        let md = bundle.format_markdown();
        assert!(!md.contains("Token Reduction"));
        assert!(md.contains("- `test_call` calls `hello` (in `tests/test.rs:99`)"));
        assert!(md.contains("_... +5 callers omitted_"));

        let txt = bundle.format_text();
        assert!(!txt.contains("Reduction:"));
        assert!(txt.contains("test_call -> hello (in tests/test.rs:99)"));
        assert!(txt.contains("... +5 callers omitted"));
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
                    call_line: 0,
                })
                .collect();
            let callees = (0..callee_count)
                .map(|i| CallEdge {
                    caller: "fn_callee".into(),
                    callee: format!("callee_{i}"),
                    file: "callee.rs".into(),
                    call_line: 0,
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
                unread_outline: None,
                omitted_callers: 0,
                omitted_callees: 0,
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

        #[test]
        fn proptest_pack_for_hook_invariants(
            target in "[a-zA-Z0-9_/.-]{1,50}",
            skeleton in "[a-zA-Z0-9_\n\t ;:()]{0,500}",
            caller_count in 0usize..30,
            callee_count in 0usize..30,
            contract_count in 0usize..15,
            outline_line_count in 0usize..40,
            coverage_tier in 0u8..3,
            has_cb in any::<bool>(),
        ) {
            let coverage = match coverage_tier {
                0 => ReadCoverage::Micro,
                1 => ReadCoverage::FullFile,
                _ => ReadCoverage::PartialMacro,
            };

            let callers: Vec<CallEdge> = (0..caller_count)
                .map(|i| CallEdge {
                    caller: format!("caller_{i}"),
                    callee: "fn_callee".into(),
                    file: format!("caller_{i}.rs"),
                    call_line: i % 50,
                })
                .collect();

            let callees: Vec<CallEdge> = (0..callee_count)
                .map(|i| CallEdge {
                    caller: "fn_caller".into(),
                    callee: format!("callee_{i}"),
                    file: format!("callee_{i}.rs"),
                    call_line: i % 50,
                })
                .collect();

            let contracts: Vec<ContractInfo> = (0..contract_count)
                .map(|i| ContractInfo {
                    trait_name: format!("Trait{i}"),
                    target_type: format!("Type{i}"),
                    methods: vec![format!("method_{i}")],
                    file: format!("contract_{i}.rs"),
                })
                .collect();

            let unread_outline = if outline_line_count > 0 {
                let mut s = String::new();
                for i in 0..outline_line_count {
                    s.push_str(&format!("L{}: pub fn outline_fn_{i}();\n", i * 10 + 1));
                }
                Some(s)
            } else {
                None
            };

            let cross_boundary = if has_cb {
                Some(CrossBoundaryImpact {
                    symbol: "ServiceDef".into(),
                    rust_definition: Some("src/svc.rs:10".into()),
                    ts_interface: Some("frontend/api.ts:25".into()),
                    go_definition: None,
                    api_call_sites: vec!["fetch('/api/v1')".into(), "fetch('/api/v2')".into()],
                })
            } else {
                None
            };

            let bundle = DistillBundle {
                target,
                skeleton,
                contracts,
                callers,
                callees,
                token_reduction_pct: 42.0,
                cross_boundary,
                unread_outline,
                omitted_callers: 0,
                omitted_callees: 0,
            };

            let packed = bundle.pack_for_hook(coverage);

            // Invariant 1: Wire character limit enforced
            prop_assert!(packed.wire_chars <= MAX_HOOK_WIRE_CHARS);
            prop_assert_eq!(packed.wire_chars, packed.markdown.chars().count());

            // Invariant 2: Fences are balanced
            prop_assert_eq!(packed.markdown.matches("```").count() % 2, 0);

            // Invariant 3: Zero "Token Reduction" in output
            prop_assert!(!packed.markdown.contains("Token Reduction"));

            // Invariant 4: Micro reads are empty
            if coverage == ReadCoverage::Micro {
                prop_assert!(packed.markdown.is_empty());
                prop_assert_eq!(packed.wire_chars, 0);
                prop_assert_eq!(packed.disposition, AdvisoryDisposition::OmittedMicro);
            }

            // Invariant 5: FullFile never includes skeleton
            if coverage == ReadCoverage::FullFile {
                prop_assert!(!packed.markdown.contains("## Type Skeleton & Interface Contracts"));
            }
        }
    }
}
