//! Agent read span models, vendor defaults, byte calculation, and hook `stdin` parsing.
//!
//! Models the line and byte windows requested by AI coding agents (Claude Code,
//! Antigravity / AGY, or generic tools) during file reads so that daemon telemetry
//! and token-saving calculations reflect actual LLM context consumption rather than
//! assuming full-file reads.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::path::PathBuf;

/// Default maximum token count returned by Claude Code `Read` tool invocations.
///
/// Source: <https://code.claude.com/docs/en/tools-reference#large-files>
pub const DEFAULT_CLAUDE_MAX_READ_TOKENS: u64 = 25_000;

/// Maximum file size (256 KB) allowed by Claude Code `Read` tool for whole-file reads.
/// Files exceeding this limit are refused unless `allow_large` is enabled.
///
/// Source: <https://code.claude.com/docs/en/tools-reference#large-files>
pub const CLAUDE_WHOLE_FILE_MAX_BYTES: u64 = 262_144;

/// Estimated bytes per token (4 bytes/token) used for LLM context budget estimates.
pub const EST_BYTES_PER_TOKEN: u64 = 4;

/// Default 1-indexed starting line for Claude Code `Read` tool invocations.
pub const DEFAULT_CLAUDE_OFFSET: usize = 1;

/// Default line count limit for Claude Code `Read` tool invocations when omitted.
///
/// Deprecated: Claude Code does not enforce a line cap when omitted; reads default to EOF bounded by token cap.
#[deprecated(
    since = "0.3.4",
    note = "Claude Code does not enforce a line cap when omitted; reads default to EOF bounded by token cap"
)]
#[allow(deprecated)]
pub const DEFAULT_CLAUDE_LIMIT: usize = 2000;

/// Default 1-indexed starting line for Antigravity (AGY) `read_file` invocations.
pub const DEFAULT_AGY_START_LINE: usize = 1;

/// Default 1-indexed ending line for Antigravity (AGY) `read_file` invocations when omitted.
///
/// Note: first-party tool schema; not published on vendor website as of 2026-10-23; applied as a secondary cap.
pub const DEFAULT_AGY_END_LINE: usize = 800;

/// Maximum byte cap per `read_file` call in Antigravity (AGY) (`46,080` bytes / 45 KiB).
///
/// Note: first-party tool schema; not published on vendor website as of 2026-10-23; applied as a secondary cap.
pub const DEFAULT_AGY_BYTE_LIMIT: u64 = 46_080;

/// Default 1-indexed starting line for OpenCode read invocations.
pub const DEFAULT_OPENCODE_OFFSET: usize = 1;

/// Default line count limit for OpenCode read invocations when omitted.
pub const DEFAULT_OPENCODE_LIMIT: usize = 2000;

/// Maximum number of lines in a bounded read to qualify as a [`ReadKind::Micro`] read.
pub const MICRO_READ_MAX_LINES: usize = 150;

/// Classification of an agent read operation as macro (structural/broad) or micro (focused/bounded).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadKind {
    /// Broad or unbounded structural read sweep.
    #[serde(alias = "Macro")]
    Macro,
    /// Focused or bounded line inspection (<= 150 lines).
    #[serde(alias = "Micro")]
    Micro,
}

impl fmt::Display for ReadKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Macro => write!(f, "macro"),
            Self::Micro => write!(f, "micro"),
        }
    }
}

/// Classification of agent read operations into advisory delivery tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadCoverage {
    /// Bounded read with effective line count <= 150 lines.
    #[serde(alias = "Micro")]
    Micro,
    /// Whole-file read where lines 1..=total_lines and all bytes were ingested without truncation.
    #[serde(alias = "FullFile", alias = "full_file")]
    FullFile,
    /// Broad read (> 150 lines) covering a subset of the file, or truncated by a vendor cap.
    #[serde(alias = "PartialMacro", alias = "partial_macro")]
    PartialMacro,
}

impl fmt::Display for ReadCoverage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Micro => write!(f, "micro"),
            Self::FullFile => write!(f, "full_file"),
            Self::PartialMacro => write!(f, "partial_macro"),
        }
    }
}

/// AI coding agent vendor identity used to apply default read window bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentVendor {
    /// Claude Code (`Read` tool: defaults to lines 1..=2000).
    Claude,
    /// Google Antigravity (`read_file` tool: defaults to lines 1..=800, capped at 46,080 bytes).
    Agy,
    /// OpenCode tool invocation (`read` tool: defaults to lines 1..=2000).
    #[value(name = "opencode")]
    #[serde(rename = "opencode", alias = "open_code", alias = "OpenCode")]
    OpenCode,
    /// Generic or unattributed tool invocation.
    #[serde(other)]
    Generic,
}

impl fmt::Display for AgentVendor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Claude => write!(f, "claude"),
            Self::Agy => write!(f, "agy"),
            Self::OpenCode => write!(f, "opencode"),
            Self::Generic => write!(f, "generic"),
        }
    }
}

impl std::str::FromStr for AgentVendor {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "claude" => Ok(Self::Claude),
            "agy" | "antigravity" => Ok(Self::Agy),
            "opencode" | "open_code" | "open-code" => Ok(Self::OpenCode),
            _ => Ok(Self::Generic),
        }
    }
}

/// Line and byte window parameters for an agent file read operation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadSpan {
    /// 1-indexed inclusive start line (AGY `StartLine` or CLI `--start-line`).
    pub start_line: Option<usize>,
    /// 1-indexed inclusive end line (AGY `EndLine` or CLI `--end-line`).
    pub end_line: Option<usize>,
    /// 1-indexed starting line offset (Claude `offset` or CLI `--offset`).
    pub offset: Option<usize>,
    /// Maximum number of lines to read (Claude `limit` or CLI `--limit`).
    pub limit: Option<usize>,
    /// Byte offset into the selected line range (AGY `ByteOffset`).
    pub byte_offset: Option<u64>,
    /// Whether large file reads are allowed beyond standard limits (Claude `allow_large`).
    #[serde(default)]
    pub allow_large: Option<bool>,
    /// Detected or explicitly specified agent vendor.
    pub vendor: Option<AgentVendor>,
}

impl ReadSpan {
    /// Returns `true` if any span parameter or vendor is populated.
    #[must_use]
    pub const fn is_specified(&self) -> bool {
        self.start_line.is_some()
            || self.end_line.is_some()
            || self.offset.is_some()
            || self.limit.is_some()
            || self.byte_offset.is_some()
            || self.allow_large.is_some()
            || self.vendor.is_some()
    }

    /// Merges non-`None` fields from `overrides` on top of `self`.
    #[must_use]
    pub const fn with_overrides(mut self, overrides: &Self) -> Self {
        if overrides.start_line.is_some() {
            self.start_line = overrides.start_line;
        }
        if overrides.end_line.is_some() {
            self.end_line = overrides.end_line;
        }
        if overrides.offset.is_some() {
            self.offset = overrides.offset;
        }
        if overrides.limit.is_some() {
            self.limit = overrides.limit;
        }
        if overrides.byte_offset.is_some() {
            self.byte_offset = overrides.byte_offset;
        }
        if overrides.allow_large.is_some() {
            self.allow_large = overrides.allow_large;
        }
        if overrides.vendor.is_some() {
            self.vendor = overrides.vendor;
        }
        self
    }

    /// Resolves the unconsolidated 1-indexed `(start_line, end_line)` before clamping to file length.
    const fn raw_bounds(&self, total_lines: usize) -> (usize, usize) {
        let start = if let Some(s) = self.start_line {
            if s == 0 { 1 } else { s }
        } else if let Some(o) = self.offset {
            if o == 0 { 1 } else { o }
        } else {
            match self.vendor {
                Some(AgentVendor::Claude) => DEFAULT_CLAUDE_OFFSET,
                Some(AgentVendor::Agy) => DEFAULT_AGY_START_LINE,
                Some(AgentVendor::OpenCode) => DEFAULT_OPENCODE_OFFSET,
                Some(AgentVendor::Generic) | None => 1,
            }
        };

        let end = if let Some(e) = self.end_line {
            if e < start { start } else { e }
        } else if let Some(l) = self.limit {
            if l == 0 {
                start
            } else {
                start.saturating_add(l.saturating_sub(1))
            }
        } else {
            match self.vendor {
                Some(AgentVendor::Claude) | Some(AgentVendor::Generic) | None => {
                    if total_lines > start {
                        total_lines
                    } else {
                        start
                    }
                }
                Some(AgentVendor::Agy) => {
                    start.saturating_add(DEFAULT_AGY_END_LINE.saturating_sub(1))
                }
                Some(AgentVendor::OpenCode) => {
                    start.saturating_add(DEFAULT_OPENCODE_LIMIT.saturating_sub(1))
                }
            }
        };

        (start, end)
    }

    /// Returns the effective line count of this read span if computable.
    #[must_use]
    pub fn effective_line_count(&self, total_lines: Option<usize>) -> Option<usize> {
        if let Some(total) = total_lines {
            if total == 0 {
                return Some(0);
            }
            let (start, end) = self.effective_range(total);
            if start == 0 {
                return Some(0);
            }
            Some(end.saturating_sub(start).saturating_add(1))
        } else if let Some(limit) = self.limit {
            Some(limit)
        } else if let (Some(start), Some(end)) = (self.start_line, self.end_line) {
            let s = if start == 0 { 1 } else { start };
            let e = if end < s { s } else { end };
            Some(e.saturating_sub(s).saturating_add(1))
        } else {
            None
        }
    }

    /// Resolves the effective 1-indexed inclusive `(start_line, end_line)` clamped to `total_lines`.
    ///
    /// Returns `(0, 0)` when `total_lines == 0` or when the requested start line exceeds `total_lines`.
    #[must_use]
    pub fn effective_range(&self, total_lines: usize) -> (usize, usize) {
        if total_lines == 0 {
            return (0, 0);
        }
        let (raw_start, raw_end) = self.raw_bounds(total_lines);
        if raw_start > total_lines {
            return (0, 0);
        }
        let end = raw_end.min(total_lines).max(raw_start);
        (raw_start, end)
    }

    /// Calculates the exact number of bytes read from `content` under this [`ReadSpan`],
    /// applying vendor-specific line defaults and byte caps.
    #[must_use]
    pub fn calculate_read_bytes(&self, content: &str) -> u64 {
        if content.is_empty() {
            return 0;
        }

        let lines: Vec<&str> = content.split_inclusive('\n').collect();
        let (start, end) = self.effective_range(lines.len());
        if start == 0 {
            return 0;
        }

        let window_bytes: u64 = lines[start - 1..end]
            .iter()
            .map(|line| line.len() as u64)
            .sum();

        let after_offset = window_bytes.saturating_sub(self.byte_offset.unwrap_or(0));

        if self.vendor == Some(AgentVendor::Agy) {
            after_offset.min(DEFAULT_AGY_BYTE_LIMIT)
        } else {
            after_offset
        }
    }

    /// Classifies this read operation as either a [`ReadKind::Macro`] sweep or [`ReadKind::Micro`] inspection.
    ///
    /// Rules:
    /// - If `total_lines == 0` or bounds are omitted (`offset`, `limit`, `start_line`, and `end_line` are all `None`), returns [`ReadKind::Macro`].
    /// - If effective lines read >= `total_lines`, returns [`ReadKind::Macro`].
    /// - If bounded intent and effective lines read <= [`MICRO_READ_MAX_LINES`], returns [`ReadKind::Micro`].
    /// - Otherwise (bounded intent > 150 lines), returns [`ReadKind::Macro`].
    #[must_use]
    pub fn classify(&self, total_lines: usize) -> ReadKind {
        if total_lines == 0 {
            return ReadKind::Macro;
        }

        let bounds_omitted = self.offset.is_none()
            && self.limit.is_none()
            && self.start_line.is_none()
            && self.end_line.is_none();

        if bounds_omitted {
            return ReadKind::Macro;
        }

        let (start, end) = self.effective_range(total_lines);
        if start == 0 {
            return ReadKind::Macro;
        }

        let lines_read = end.saturating_sub(start).saturating_add(1);
        if lines_read >= total_lines {
            return ReadKind::Macro;
        }

        if lines_read <= MICRO_READ_MAX_LINES {
            ReadKind::Micro
        } else {
            ReadKind::Macro
        }
    }
}

/// Parsed representation of an agent tool hook payload from `stdin`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedHookInput {
    /// Target file path extracted from the hook payload, if present.
    pub file_path: Option<PathBuf>,
    /// Extracted line/byte span and detected vendor.
    pub span: ReadSpan,
    /// Whether the tool execution resulted in an error (e.g. Antigravity PostToolUse error).
    #[serde(default)]
    pub tool_error: bool,
}

/// Parses a JSON hook payload from Claude Code or Antigravity (AGY) `stdin`.
///
/// Supported formats:
/// - **Claude Code**: `{"tool_name": "Read", "tool_input": {"file_path": "...", "offset": 100, "limit": 50}}`
/// - **Antigravity (AGY)**: `{"toolCall": {"name": "read_file", "args": {"Path": "...", "StartLine": 50, "EndLine": 80, "ByteOffset": 0}}}`
/// - **Flat / Hybrid JSON**: Top-level fields with `file_path`/`Path`, `offset`/`limit`, or `StartLine`/`EndLine`.
#[must_use]
pub fn parse_hook_stdin(input: &str) -> Option<ParsedHookInput> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let root: Value = serde_json::from_str(trimmed).ok()?;
    let obj = root.as_object()?;

    if let Some(tool_call) = obj
        .get("toolCall")
        .or_else(|| obj.get("tool_call"))
        .and_then(Value::as_object)
    {
        return parse_agy_tool_call(tool_call, obj);
    }

    if let Some(tool_input) = obj
        .get("tool_input")
        .or_else(|| obj.get("toolInput"))
        .and_then(Value::as_object)
    {
        return parse_tool_input_envelope(obj, tool_input);
    }

    parse_flat_object(obj)
}

fn parse_agy_tool_call(
    tool_call: &serde_json::Map<String, Value>,
    root: &serde_json::Map<String, Value>,
) -> Option<ParsedHookInput> {
    let args = tool_call
        .get("args")
        .or_else(|| tool_call.get("arguments"))
        .or_else(|| tool_call.get("parameters"))
        .or_else(|| tool_call.get("input"))
        .and_then(Value::as_object)
        .unwrap_or(tool_call);

    let file_path = extract_path(args).or_else(|| extract_path(root));
    let mut span = extract_span_fields(args);
    span.vendor = extract_explicit_vendor(args)
        .or_else(|| extract_explicit_vendor(root))
        .or(Some(AgentVendor::Agy));

    let tool_error = has_error(root) || has_error(tool_call) || has_error(args);
    Some(ParsedHookInput {
        file_path,
        span,
        tool_error,
    })
}

fn parse_tool_input_envelope(
    root: &serde_json::Map<String, Value>,
    tool_input: &serde_json::Map<String, Value>,
) -> Option<ParsedHookInput> {
    let tool_name = root
        .get("tool_name")
        .or_else(|| root.get("toolName"))
        .and_then(Value::as_str)
        .unwrap_or("");

    let is_agy_tool = tool_name.eq_ignore_ascii_case("read_file")
        || tool_name.ends_with(":read_file")
        || tool_name.eq_ignore_ascii_case("view_file")
        || tool_name.ends_with(":view_file")
        || tool_input.contains_key("AbsolutePath")
        || tool_input.contains_key("Path")
        || tool_input.contains_key("StartLine")
        || tool_input.contains_key("EndLine")
        || tool_input.contains_key("ByteOffset");

    let file_path = extract_path(tool_input).or_else(|| extract_path(root));
    let mut span = extract_span_fields(tool_input);
    let is_opencode_tool = tool_input.contains_key("filePath")
        && !tool_input.contains_key("file_path")
        && !is_agy_tool;
    span.vendor = extract_explicit_vendor(tool_input)
        .or_else(|| extract_explicit_vendor(root))
        .or(Some(if is_agy_tool {
            AgentVendor::Agy
        } else if is_opencode_tool {
            AgentVendor::OpenCode
        } else {
            AgentVendor::Claude
        }));

    let tool_error = has_error(root) || has_error(tool_input);
    Some(ParsedHookInput {
        file_path,
        span,
        tool_error,
    })
}

fn parse_flat_object(obj: &serde_json::Map<String, Value>) -> Option<ParsedHookInput> {
    let file_path = extract_path(obj);
    let mut span = extract_span_fields(obj);
    let tool_name = obj
        .get("tool_name")
        .or_else(|| obj.get("toolName"))
        .or_else(|| obj.get("name"))
        .and_then(Value::as_str);

    let inferred_vendor = extract_explicit_vendor(obj).or_else(|| {
        if obj.contains_key("AbsolutePath")
            || obj.contains_key("Path")
            || obj.contains_key("StartLine")
            || obj.contains_key("EndLine")
            || obj.contains_key("ByteOffset")
        {
            Some(AgentVendor::Agy)
        } else if obj.contains_key("filePath") && !obj.contains_key("file_path") {
            Some(AgentVendor::OpenCode)
        } else if let Some(name) = tool_name {
            if name.eq_ignore_ascii_case("read") {
                return Some(AgentVendor::Claude);
            }
            if name.eq_ignore_ascii_case("read_file")
                || name.ends_with(":read_file")
                || name.eq_ignore_ascii_case("view_file")
                || name.ends_with(":view_file")
            {
                return Some(AgentVendor::Agy);
            }
            None
        } else if obj.contains_key("offset")
            || obj.contains_key("limit")
            || obj.contains_key("file_path")
        {
            Some(AgentVendor::Claude)
        } else {
            None
        }
    });

    span.vendor = inferred_vendor;

    let tool_error = has_error(obj);
    if file_path.is_none() && !span.is_specified() && !tool_error {
        return None;
    }

    Some(ParsedHookInput {
        file_path,
        span,
        tool_error,
    })
}

fn extract_path(map: &serde_json::Map<String, Value>) -> Option<PathBuf> {
    [
        "file_path",
        "AbsolutePath",
        "Path",
        "path",
        "filePath",
        "file",
    ]
    .iter()
    .find_map(|key| map.get(*key).and_then(Value::as_str))
    .filter(|s| !s.is_empty())
    .map(PathBuf::from)
}

fn extract_bool(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|key| {
        let val = map.get(*key)?;
        val.as_bool().or_else(|| val.as_str()?.parse::<bool>().ok())
    })
}

fn has_error(map: &serde_json::Map<String, Value>) -> bool {
    map.get("error")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
}

fn extract_usize(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<usize> {
    keys.iter().find_map(|key| {
        let val = map.get(*key)?;
        val.as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .or_else(|| val.as_str()?.parse::<usize>().ok())
    })
}

fn extract_u64(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        let val = map.get(*key)?;
        val.as_u64().or_else(|| val.as_str()?.parse::<u64>().ok())
    })
}

fn extract_explicit_vendor(map: &serde_json::Map<String, Value>) -> Option<AgentVendor> {
    let raw = map.get("vendor").and_then(Value::as_str)?;
    if raw.eq_ignore_ascii_case("claude") {
        Some(AgentVendor::Claude)
    } else if raw.eq_ignore_ascii_case("agy") || raw.eq_ignore_ascii_case("antigravity") {
        Some(AgentVendor::Agy)
    } else if raw.eq_ignore_ascii_case("opencode") || raw.eq_ignore_ascii_case("open_code") {
        Some(AgentVendor::OpenCode)
    } else if raw.eq_ignore_ascii_case("generic") {
        Some(AgentVendor::Generic)
    } else {
        None
    }
}

fn extract_span_fields(map: &serde_json::Map<String, Value>) -> ReadSpan {
    ReadSpan {
        start_line: extract_usize(map, &["StartLine", "start_line", "startLine"]),
        end_line: extract_usize(map, &["EndLine", "end_line", "endLine"]),
        offset: extract_usize(map, &["offset", "Offset"]),
        limit: extract_usize(map, &["limit", "Limit"]),
        byte_offset: extract_u64(map, &["ByteOffset", "byte_offset", "byteOffset"]),
        allow_large: extract_bool(map, &["allow_large", "allowLarge"]),
        vendor: None,
    }
}

/// Caller-resolved environment inputs for read-budget calculation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadBudgetEnv {
    /// Vendor detected from the process environment (lowest-precedence signal).
    pub env_vendor: Option<AgentVendor>,
    /// `CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS` override, if set and > 0.
    pub claude_max_read_tokens: Option<u64>,
}

impl ReadBudgetEnv {
    /// Returns the maximum allowed read bytes for Claude Code based on the token cap.
    #[must_use]
    pub fn claude_max_bytes(&self) -> u64 {
        self.claude_max_read_tokens
            .unwrap_or(DEFAULT_CLAUDE_MAX_READ_TOKENS)
            .saturating_mul(EST_BYTES_PER_TOKEN)
    }
}

/// Detects [`ReadBudgetEnv`] from process environment variables using a closure.
#[must_use]
pub fn detect_budget_env(get: impl Fn(&str) -> Option<String>) -> ReadBudgetEnv {
    let env_vendor = if get("CLAUDE_CODE_CHILD_SESSION").as_deref() == Some("1")
        || get("CLAUDECODE").as_deref() == Some("1")
    {
        Some(AgentVendor::Claude)
    } else if get("ANTIGRAVITY_AGENT").as_deref() == Some("1")
        || get("ANTIGRAVITY_CONVERSATION_ID")
            .as_deref()
            .is_some_and(|s| !s.is_empty())
        || get("ANTIGRAVITY_AGENTAPI_EXE")
            .as_deref()
            .is_some_and(|s| !s.is_empty())
    {
        Some(AgentVendor::Agy)
    } else if get("OPENCODE")
        .as_deref()
        .is_some_and(|s| s == "1" || (!s.is_empty() && s != "0"))
        || get("OPENCODE_AGENT").as_deref() == Some("1")
        || get("OPENCODE_SESSION_ID")
            .as_deref()
            .is_some_and(|s| !s.is_empty() && s != "0")
        || get("OPENCODE_PID")
            .as_deref()
            .is_some_and(|s| !s.is_empty() && s != "0")
        || get("OPENCODE_CLIENT")
            .as_deref()
            .is_some_and(|s| !s.is_empty() && s != "0")
    {
        Some(AgentVendor::OpenCode)
    } else {
        None
    };

    let claude_max_read_tokens = get("CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS")
        .and_then(|val| val.trim().parse::<u64>().ok())
        .filter(|&tokens| tokens > 0);

    ReadBudgetEnv {
        env_vendor,
        claude_max_read_tokens,
    }
}

fn claude_budget(span: &ReadSpan, content: &str, env: &ReadBudgetEnv) -> u64 {
    let file_bytes = content.len() as u64;
    let has_bounds = span.offset.is_some()
        || span.limit.is_some()
        || span.start_line.is_some()
        || span.end_line.is_some();

    if span.allow_large == Some(true) {
        return if has_bounds {
            span.calculate_read_bytes(content)
        } else {
            file_bytes
        };
    }

    let token_byte_cap = env.claude_max_bytes();

    if !has_bounds {
        let mut claude_span = span.clone();
        claude_span.vendor = Some(AgentVendor::Claude);
        claude_span
            .calculate_read_bytes(content)
            .min(token_byte_cap)
    } else {
        let mut claude_span = span.clone();
        claude_span.vendor = Some(AgentVendor::Claude);
        let window_bytes = claude_span.calculate_read_bytes(content);
        if window_bytes > token_byte_cap {
            0
        } else {
            window_bytes
        }
    }
}

/// Resolves the effective byte read budget requested by an agent file read.
///
/// Implements vendor-specific defaults and caps:
/// - **Claude**: 25,000 tokens (or override), auto-paginated up to token cap for unbounded reads.
/// - **Antigravity**: 800 lines default, 46,080 byte maximum cap.
/// - **OpenCode**: 2,000 lines default.
/// - **Generic**: Full file size (or explicit line window).
#[must_use]
pub fn resolve_read_budget(span: &ReadSpan, content: &str, env: &ReadBudgetEnv) -> u64 {
    if content.is_empty() {
        return 0;
    }

    let vendor = span
        .vendor
        .or(env.env_vendor)
        .unwrap_or(AgentVendor::Generic);
    match vendor {
        AgentVendor::Claude => claude_budget(span, content, env),
        AgentVendor::Agy => {
            let mut agy_span = span.clone();
            agy_span.vendor = Some(AgentVendor::Agy);
            agy_span.calculate_read_bytes(content)
        }
        AgentVendor::OpenCode => {
            let mut opencode_span = span.clone();
            opencode_span.vendor = Some(AgentVendor::OpenCode);
            opencode_span.calculate_read_bytes(content)
        }
        AgentVendor::Generic => {
            let mut gen_span = span.clone();
            gen_span.vendor = Some(AgentVendor::Generic);
            gen_span.calculate_read_bytes(content)
        }
    }
}

/// Classifies an agent read operation into one of three advisory delivery tiers:
/// [`ReadCoverage::Micro`], [`ReadCoverage::FullFile`], or [`ReadCoverage::PartialMacro`].
#[must_use]
pub fn classify_read_coverage(
    span: &ReadSpan,
    total_lines: Option<usize>,
    raw_bytes: Option<u64>,
    env: &ReadBudgetEnv,
) -> ReadCoverage {
    let vendor = span
        .vendor
        .or(env.env_vendor)
        .unwrap_or(AgentVendor::Generic);

    if let Some(tot) = total_lines {
        if tot == 0 {
            return ReadCoverage::FullFile;
        }

        let (start, end) = span.effective_range(tot);
        let covers_all_lines = start <= 1 && end >= tot;

        if covers_all_lines {
            let is_truncated = match vendor {
                AgentVendor::Claude => {
                    span.allow_large != Some(true)
                        && raw_bytes.is_some_and(|b| b > env.claude_max_bytes())
                }
                AgentVendor::Agy => raw_bytes.is_some_and(|b| b > DEFAULT_AGY_BYTE_LIMIT),
                AgentVendor::OpenCode | AgentVendor::Generic => false,
            };

            if !is_truncated {
                return ReadCoverage::FullFile;
            }
        }

        let has_explicit_bounds = span.offset.is_some()
            || span.limit.is_some()
            || span.start_line.is_some()
            || span.end_line.is_some();

        if has_explicit_bounds && start > 0 {
            let lines_read = end.saturating_sub(start).saturating_add(1);
            if lines_read <= MICRO_READ_MAX_LINES {
                return ReadCoverage::Micro;
            }
        }

        ReadCoverage::PartialMacro
    } else {
        if let Some(lines) = span.effective_line_count(None) {
            if lines <= MICRO_READ_MAX_LINES {
                return ReadCoverage::Micro;
            }
        }
        ReadCoverage::PartialMacro
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    fn build_sample_file(lines: usize, line_len: usize) -> String {
        let mut buf = String::with_capacity(lines * line_len);
        for i in 1..=lines {
            let prefix = format!("line-{i:04}: ");
            let pad_len = line_len.saturating_sub(prefix.len() + 1);
            let _ = writeln!(buf, "{prefix}{}", "x".repeat(pad_len));
        }
        buf
    }

    #[test]
    fn claude_defaults_read_to_eof_without_line_cap() {
        let span = ReadSpan {
            vendor: Some(AgentVendor::Claude),
            ..Default::default()
        };
        assert_eq!(span.effective_range(3000), (1, 3000));
        assert_eq!(span.effective_range(500), (1, 500));

        let content = build_sample_file(3000, 50);
        assert_eq!(span.calculate_read_bytes(&content), 3000 * 50);
    }

    #[test]
    fn claude_micro_read_offset_and_limit() {
        let span = ReadSpan {
            offset: Some(100),
            limit: Some(30),
            vendor: Some(AgentVendor::Claude),
            ..Default::default()
        };
        assert_eq!(span.effective_range(500), (100, 129));

        let content = build_sample_file(500, 40);
        assert_eq!(span.calculate_read_bytes(&content), 30 * 40);
    }

    #[test]
    fn agy_defaults_apply_1_to_800_and_byte_cap() {
        let span = ReadSpan {
            vendor: Some(AgentVendor::Agy),
            ..Default::default()
        };
        assert_eq!(span.effective_range(2000), (1, 800));

        // 800 lines * 30 bytes = 24,000 bytes (under 46,080 cap)
        let small_lines = build_sample_file(2000, 30);
        assert_eq!(span.calculate_read_bytes(&small_lines), 800 * 30);

        // 800 lines * 100 bytes = 80,000 bytes (capped at 46,080)
        let large_lines = build_sample_file(2000, 100);
        assert_eq!(
            span.calculate_read_bytes(&large_lines),
            DEFAULT_AGY_BYTE_LIMIT
        );
    }

    #[test]
    fn agy_micro_read_start_and_end_line() {
        let span = ReadSpan {
            start_line: Some(50),
            end_line: Some(80),
            vendor: Some(AgentVendor::Agy),
            ..Default::default()
        };
        assert_eq!(span.effective_range(500), (50, 80));

        let content = build_sample_file(500, 40);
        // 31 lines (50..=80 inclusive) * 40 bytes = 1,240 bytes
        assert_eq!(span.calculate_read_bytes(&content), 31 * 40);
    }

    #[test]
    fn parse_claude_stdin_json() {
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "tool_name": "Read",
            "tool_input": {
                "file_path": "/workspace/src/lib.rs",
                "offset": 120,
                "limit": 45
            }
        }"#;
        let parsed = parse_hook_stdin(json).expect("should parse Claude payload");
        assert_eq!(
            parsed.file_path,
            Some(PathBuf::from("/workspace/src/lib.rs"))
        );
        assert_eq!(parsed.span.vendor, Some(AgentVendor::Claude));
        assert_eq!(parsed.span.offset, Some(120));
        assert_eq!(parsed.span.limit, Some(45));
        assert_eq!(parsed.span.effective_range(500), (120, 164));
    }

    #[test]
    fn parse_claude_stdin_with_omitted_bounds() {
        let json = r#"{
            "tool_name": "Read",
            "tool_input": {
                "file_path": "/workspace/src/main.rs"
            }
        }"#;
        let parsed = parse_hook_stdin(json).expect("should parse Claude payload");
        assert_eq!(parsed.span.vendor, Some(AgentVendor::Claude));
        assert_eq!(parsed.span.offset, None);
        assert_eq!(parsed.span.limit, None);
        assert_eq!(parsed.span.effective_range(5000), (1, 5000));
    }

    #[test]
    fn parse_agy_stdin_json() {
        let json = r#"{
            "toolCall": {
                "name": "read_file",
                "args": {
                    "Path": "/workspace/src/server.rs",
                    "StartLine": 50,
                    "EndLine": 80,
                    "ByteOffset": 100
                }
            }
        }"#;
        let parsed = parse_hook_stdin(json).expect("should parse AGY payload");
        assert_eq!(
            parsed.file_path,
            Some(PathBuf::from("/workspace/src/server.rs"))
        );
        assert_eq!(parsed.span.vendor, Some(AgentVendor::Agy));
        assert_eq!(parsed.span.start_line, Some(50));
        assert_eq!(parsed.span.end_line, Some(80));
        assert_eq!(parsed.span.byte_offset, Some(100));
    }

    #[test]
    fn parse_agy_stdin_with_omitted_lines() {
        let json = r#"{
            "toolCall": {
                "name": "read_file",
                "args": {
                    "Path": "/workspace/src/server.rs"
                }
            }
        }"#;
        let parsed = parse_hook_stdin(json).expect("should parse AGY payload");
        assert_eq!(parsed.span.vendor, Some(AgentVendor::Agy));
        assert_eq!(parsed.span.start_line, None);
        assert_eq!(parsed.span.end_line, None);
        assert_eq!(parsed.span.effective_range(2000), (1, 800));
    }

    #[test]
    fn cli_overrides_merge_cleanly() {
        let base = ReadSpan {
            offset: Some(10),
            limit: Some(20),
            vendor: Some(AgentVendor::Claude),
            ..Default::default()
        };
        let cli = ReadSpan {
            limit: Some(50),
            ..Default::default()
        };
        let merged = base.with_overrides(&cli);
        assert_eq!(merged.offset, Some(10));
        assert_eq!(merged.limit, Some(50));
        assert_eq!(merged.vendor, Some(AgentVendor::Claude));
    }

    #[test]
    fn empty_or_invalid_stdin_returns_none() {
        assert!(parse_hook_stdin("").is_none());
        assert!(parse_hook_stdin("   ").is_none());
        assert!(parse_hook_stdin("not json").is_none());
        assert!(parse_hook_stdin("{}").is_none());
    }

    #[test]
    fn out_of_bounds_start_returns_zero_range_and_bytes() {
        let span = ReadSpan {
            offset: Some(600),
            limit: Some(30),
            vendor: Some(AgentVendor::Claude),
            ..Default::default()
        };
        assert_eq!(span.effective_range(500), (0, 0));
        let content = build_sample_file(500, 40);
        assert_eq!(span.calculate_read_bytes(&content), 0);
    }

    #[test]
    fn test_detect_budget_env() {
        let env_claude_1 = detect_budget_env(|k| match k {
            "CLAUDECODE" => Some("1".to_string()),
            _ => None,
        });
        assert_eq!(env_claude_1.env_vendor, Some(AgentVendor::Claude));
        assert_eq!(env_claude_1.claude_max_read_tokens, None);

        let env_child_1 = detect_budget_env(|k| match k {
            "CLAUDE_CODE_CHILD_SESSION" => Some("1".to_string()),
            _ => None,
        });
        assert_eq!(env_child_1.env_vendor, Some(AgentVendor::Claude));

        let env_zero = detect_budget_env(|k| match k {
            "CLAUDECODE" => Some("0".to_string()),
            _ => None,
        });
        assert_eq!(env_zero.env_vendor, None);

        let env_agy_agent = detect_budget_env(|k| match k {
            "ANTIGRAVITY_AGENT" => Some("1".to_string()),
            _ => None,
        });
        assert_eq!(env_agy_agent.env_vendor, Some(AgentVendor::Agy));

        let env_agy_conv = detect_budget_env(|k| match k {
            "ANTIGRAVITY_CONVERSATION_ID" => Some("conv-1234-uuid".to_string()),
            _ => None,
        });
        assert_eq!(env_agy_conv.env_vendor, Some(AgentVendor::Agy));

        let env_agy_exe = detect_budget_env(|k| match k {
            "ANTIGRAVITY_AGENTAPI_EXE" => Some("/home/mark/.local/bin/agy".to_string()),
            _ => None,
        });
        assert_eq!(env_agy_exe.env_vendor, Some(AgentVendor::Agy));

        let env_agy_empty = detect_budget_env(|k| match k {
            "ANTIGRAVITY_AGENT" => Some("0".to_string()),
            "ANTIGRAVITY_CONVERSATION_ID" => Some("".to_string()),
            "ANTIGRAVITY_AGENTAPI_EXE" => Some("".to_string()),
            _ => None,
        });
        assert_eq!(env_agy_empty.env_vendor, None);

        let env_tokens = detect_budget_env(|k| match k {
            "CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS" => Some("40000".to_string()),
            _ => None,
        });
        assert_eq!(env_tokens.claude_max_read_tokens, Some(40_000));

        for invalid in ["0", "-5", "abc", ""] {
            let env_inv = detect_budget_env(|k| match k {
                "CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS" => Some(invalid.to_string()),
                _ => None,
            });
            assert_eq!(env_inv.claude_max_read_tokens, None);
        }
    }

    #[test]
    fn test_resolve_read_budget_matrix() {
        let empty_env = ReadBudgetEnv::default();

        // Empty file -> 0
        let span_generic = ReadSpan::default();
        assert_eq!(resolve_read_budget(&span_generic, "", &empty_env), 0);

        // Claude unbounded on 300 KB file (> 256 KB) -> auto-paginates up to token cap (100,000 bytes)
        let large_300k = build_sample_file(6000, 50); // 300,000 bytes > 262,144
        let claude_unbounded = ReadSpan {
            vendor: Some(AgentVendor::Claude),
            ..Default::default()
        };
        assert_eq!(
            resolve_read_budget(&claude_unbounded, &large_300k, &empty_env),
            100_000
        );

        // Claude unbounded on 3,000-line x 50 B file (150 KB) -> min(150,000, 100,000) = 100,000
        let medium_150k = build_sample_file(3000, 50);
        assert_eq!(
            resolve_read_budget(&claude_unbounded, &medium_150k, &empty_env),
            100_000
        );

        // Claude with allow_large: true on 300 KB file -> 300,000 bytes
        let claude_large = ReadSpan {
            vendor: Some(AgentVendor::Claude),
            allow_large: Some(true),
            ..Default::default()
        };
        assert_eq!(
            resolve_read_budget(&claude_large, &large_300k, &empty_env),
            large_300k.len() as u64
        );

        // Claude offset=1, limit=5000 x 50 B (250 KB > 100 KB cap) -> 0
        let claude_too_big = ReadSpan {
            offset: Some(1),
            limit: Some(5000),
            vendor: Some(AgentVendor::Claude),
            ..Default::default()
        };
        assert_eq!(
            resolve_read_budget(&claude_too_big, &large_300k, &empty_env),
            0
        );

        // Claude with token override 10,000 -> cap 40,000
        let env_10k = ReadBudgetEnv {
            env_vendor: None,
            claude_max_read_tokens: Some(10_000),
        };
        assert_eq!(
            resolve_read_budget(&claude_unbounded, &medium_150k, &env_10k),
            40_000
        );

        // Agy with no lines on 2,000 x 100 B -> 46,080
        let agy_unbounded = ReadSpan {
            vendor: Some(AgentVendor::Agy),
            ..Default::default()
        };
        let agy_200k = build_sample_file(2000, 100);
        assert_eq!(
            resolve_read_budget(&agy_unbounded, &agy_200k, &empty_env),
            DEFAULT_AGY_BYTE_LIMIT
        );

        // Generic on any file -> file_bytes
        let gen_span = ReadSpan {
            vendor: Some(AgentVendor::Generic),
            ..Default::default()
        };
        assert_eq!(
            resolve_read_budget(&gen_span, &medium_150k, &empty_env),
            medium_150k.len() as u64
        );

        // Explicit --vendor beats env_vendor
        let env_claude = ReadBudgetEnv {
            env_vendor: Some(AgentVendor::Claude),
            claude_max_read_tokens: None,
        };
        let span_explicit_generic = ReadSpan {
            vendor: Some(AgentVendor::Generic),
            ..Default::default()
        };
        assert_eq!(
            resolve_read_budget(&span_explicit_generic, &large_300k, &env_claude),
            large_300k.len() as u64
        );
    }

    #[test]
    fn parse_agy_view_file_payload_and_error() {
        let json_ok = r#"{
            "toolCall": {
                "name": "view_file",
                "args": {
                    "AbsolutePath": "/abs/a.rs",
                    "StartLine": 1,
                    "EndLine": 20
                }
            },
            "stepIdx": 3
        }"#;
        let parsed_ok = parse_hook_stdin(json_ok).expect("parse view_file payload");
        assert_eq!(parsed_ok.file_path, Some(PathBuf::from("/abs/a.rs")));
        assert_eq!(parsed_ok.span.vendor, Some(AgentVendor::Agy));
        assert_eq!(parsed_ok.span.start_line, Some(1));
        assert_eq!(parsed_ok.span.end_line, Some(20));
        assert!(!parsed_ok.tool_error);

        let json_err = r#"{
            "toolCall": {
                "name": "view_file",
                "args": {
                    "AbsolutePath": "/abs/a.rs",
                    "StartLine": 1,
                    "EndLine": 20
                }
            },
            "stepIdx": 3,
            "error": "ENOENT"
        }"#;
        let parsed_err = parse_hook_stdin(json_err).expect("parse view_file payload with error");
        assert_eq!(parsed_err.file_path, Some(PathBuf::from("/abs/a.rs")));
        assert!(parsed_err.tool_error);
    }

    #[test]
    fn test_classify_claude_unbounded_and_bounded() {
        // Claude unbounded read -> Macro
        let unbounded = ReadSpan {
            vendor: Some(AgentVendor::Claude),
            ..Default::default()
        };
        assert_eq!(unbounded.classify(1000), ReadKind::Macro);
        assert_eq!(unbounded.classify(50), ReadKind::Macro);

        // Claude micro-read (offset: 140, limit: 30) on 500-line file -> Micro
        let micro = ReadSpan {
            offset: Some(140),
            limit: Some(30),
            vendor: Some(AgentVendor::Claude),
            ..Default::default()
        };
        assert_eq!(micro.classify(500), ReadKind::Micro);

        // Claude large bounded read (limit: 2000) on 5000-line file -> Macro
        let large_bounded = ReadSpan {
            offset: Some(1),
            limit: Some(2000),
            vendor: Some(AgentVendor::Claude),
            ..Default::default()
        };
        assert_eq!(large_bounded.classify(5000), ReadKind::Macro);
    }

    #[test]
    fn test_classify_agy_unbounded_and_bounded() {
        // AGY unbounded read (StartLine/EndLine omitted) -> Macro
        let unbounded = ReadSpan {
            vendor: Some(AgentVendor::Agy),
            ..Default::default()
        };
        assert_eq!(unbounded.classify(1000), ReadKind::Macro);

        // AGY micro-read (StartLine: 40, EndLine: 60 = 21 lines) on 200-line file -> Micro
        let micro = ReadSpan {
            start_line: Some(40),
            end_line: Some(60),
            vendor: Some(AgentVendor::Agy),
            ..Default::default()
        };
        assert_eq!(micro.classify(200), ReadKind::Micro);

        // AGY bounded read exceeding 150 lines (StartLine: 1, EndLine: 160) on 500-line file -> Macro
        let large = ReadSpan {
            start_line: Some(1),
            end_line: Some(160),
            vendor: Some(AgentVendor::Agy),
            ..Default::default()
        };
        assert_eq!(large.classify(500), ReadKind::Macro);
    }

    #[test]
    fn test_classify_small_files_and_edge_cases() {
        // Whole-file read on small file (15 lines, bounded read 1..=15) -> Macro
        let small_full = ReadSpan {
            start_line: Some(1),
            end_line: Some(15),
            ..Default::default()
        };
        assert_eq!(small_full.classify(15), ReadKind::Macro);

        // Whole-file read via offset/limit on small file (<= 20 lines) -> Macro
        let small_full_limit = ReadSpan {
            offset: Some(1),
            limit: Some(20),
            ..Default::default()
        };
        assert_eq!(small_full_limit.classify(20), ReadKind::Macro);
        assert_eq!(small_full_limit.classify(10), ReadKind::Macro);

        // Partial bounded read on small file (lines 1..=5 of 20) -> Micro
        let small_partial = ReadSpan {
            start_line: Some(1),
            end_line: Some(5),
            ..Default::default()
        };
        assert_eq!(small_partial.classify(20), ReadKind::Micro);

        // Zero total lines -> Macro
        assert_eq!(small_partial.classify(0), ReadKind::Macro);
        assert_eq!(ReadSpan::default().classify(0), ReadKind::Macro);

        // Exactly 150 lines read on a larger file -> Micro
        let exactly_150 = ReadSpan {
            start_line: Some(1),
            end_line: Some(150),
            ..Default::default()
        };
        assert_eq!(exactly_150.classify(500), ReadKind::Micro);

        // 151 lines read on a larger file -> Macro
        let exactly_151 = ReadSpan {
            start_line: Some(1),
            end_line: Some(151),
            ..Default::default()
        };
        assert_eq!(exactly_151.classify(500), ReadKind::Macro);
    }

    #[test]
    fn test_read_kind_serde_and_display() {
        assert_eq!(ReadKind::Macro.to_string(), "macro");
        assert_eq!(ReadKind::Micro.to_string(), "micro");

        let json_macro = serde_json::to_string(&ReadKind::Macro).unwrap();
        assert_eq!(json_macro, "\"macro\"");

        let de_macro: ReadKind = serde_json::from_str("\"macro\"").unwrap();
        assert_eq!(de_macro, ReadKind::Macro);

        let de_macro_alias: ReadKind = serde_json::from_str("\"Macro\"").unwrap();
        assert_eq!(de_macro_alias, ReadKind::Macro);

        let de_micro_alias: ReadKind = serde_json::from_str("\"Micro\"").unwrap();
        assert_eq!(de_micro_alias, ReadKind::Micro);
    }

    #[test]
    fn opencode_defaults_read_to_2000_lines() {
        let span = ReadSpan {
            vendor: Some(AgentVendor::OpenCode),
            ..Default::default()
        };
        assert_eq!(span.effective_range(3000), (1, 2000));
        assert_eq!(span.effective_range(1500), (1, 1500));
        assert_eq!(span.effective_range(0), (0, 0));
    }

    #[test]
    fn parse_opencode_stdin_json() {
        let json_envelope = r#"{
            "tool_name": "read",
            "tool_input": {
                "filePath": "/workspace/src/lib.rs",
                "offset": 100,
                "limit": 50
            }
        }"#;
        let parsed = parse_hook_stdin(json_envelope).expect("parse opencode envelope");
        assert_eq!(
            parsed.file_path,
            Some(PathBuf::from("/workspace/src/lib.rs"))
        );
        assert_eq!(parsed.span.vendor, Some(AgentVendor::OpenCode));
        assert_eq!(parsed.span.offset, Some(100));
        assert_eq!(parsed.span.limit, Some(50));
        assert_eq!(parsed.span.effective_range(500), (100, 149));

        let json_flat = r#"{
            "filePath": "/workspace/src/main.rs",
            "offset": 1,
            "limit": 2000
        }"#;
        let parsed_flat = parse_hook_stdin(json_flat).expect("parse opencode flat");
        assert_eq!(parsed_flat.span.vendor, Some(AgentVendor::OpenCode));
        assert_eq!(
            parsed_flat.file_path,
            Some(PathBuf::from("/workspace/src/main.rs"))
        );

        let json_explicit = r#"{
            "vendor": "opencode",
            "file_path": "/workspace/src/app.rs"
        }"#;
        let parsed_exp = parse_hook_stdin(json_explicit).expect("parse opencode explicit vendor");
        assert_eq!(parsed_exp.span.vendor, Some(AgentVendor::OpenCode));
    }

    #[test]
    fn test_detect_budget_env_opencode() {
        for (var, val) in [
            ("OPENCODE", "1"),
            ("OPENCODE_AGENT", "1"),
            ("OPENCODE_SESSION_ID", "sess-1234"),
            ("OPENCODE_PID", "9876"),
            ("OPENCODE_CLIENT", "desktop"),
        ] {
            let env = detect_budget_env(|k| {
                if k == var {
                    Some(val.to_string())
                } else {
                    None
                }
            });
            assert_eq!(
                env.env_vendor,
                Some(AgentVendor::OpenCode),
                "failed for {var}"
            );
        }

        let env_zero = detect_budget_env(|k| {
            if k == "OPENCODE" {
                Some("0".to_string())
            } else {
                None
            }
        });
        assert_eq!(env_zero.env_vendor, None);

        let env_empty = detect_budget_env(|k| {
            if k == "OPENCODE_SESSION_ID" {
                Some(String::new())
            } else {
                None
            }
        });
        assert_eq!(env_empty.env_vendor, None);
    }

    #[test]
    fn test_classify_read_coverage_opencode() {
        let env = ReadBudgetEnv::default();
        let unbounded = ReadSpan {
            vendor: Some(AgentVendor::OpenCode),
            ..Default::default()
        };

        // OpenCode on 1500-line file covers whole file (1500 <= 2000) -> FullFile
        assert_eq!(
            classify_read_coverage(&unbounded, Some(1500), None, &env),
            ReadCoverage::FullFile
        );

        // OpenCode on 2500-line file caps at 2000 lines -> PartialMacro
        assert_eq!(
            classify_read_coverage(&unbounded, Some(2500), None, &env),
            ReadCoverage::PartialMacro
        );

        // OpenCode bounded micro-read (50 lines) on 2500-line file -> Micro
        let micro = ReadSpan {
            offset: Some(10),
            limit: Some(50),
            vendor: Some(AgentVendor::OpenCode),
            ..Default::default()
        };
        assert_eq!(
            classify_read_coverage(&micro, Some(2500), None, &env),
            ReadCoverage::Micro
        );
    }

    #[test]
    fn test_classify_read_coverage_micro_boundaries() {
        let env = ReadBudgetEnv::default();

        // Exactly 150 lines on 500-line file -> Micro
        let span_150 = ReadSpan {
            start_line: Some(1),
            end_line: Some(150),
            ..Default::default()
        };
        assert_eq!(
            classify_read_coverage(&span_150, Some(500), None, &env),
            ReadCoverage::Micro
        );

        // 151 lines on 500-line file -> PartialMacro
        let span_151 = ReadSpan {
            start_line: Some(1),
            end_line: Some(151),
            ..Default::default()
        };
        assert_eq!(
            classify_read_coverage(&span_151, Some(500), None, &env),
            ReadCoverage::PartialMacro
        );

        // 150 lines on 150-line file covers whole file -> FullFile
        assert_eq!(
            classify_read_coverage(&span_150, Some(150), None, &env),
            ReadCoverage::FullFile
        );

        // 20 lines on 20-line file -> FullFile
        let span_20 = ReadSpan {
            start_line: Some(1),
            end_line: Some(20),
            ..Default::default()
        };
        assert_eq!(
            classify_read_coverage(&span_20, Some(20), None, &env),
            ReadCoverage::FullFile
        );

        // 0 lines -> FullFile
        assert_eq!(
            classify_read_coverage(&span_20, Some(0), None, &env),
            ReadCoverage::FullFile
        );
    }

    #[test]
    fn test_claude_max_bytes_and_effective_line_count() {
        let env_default = ReadBudgetEnv::default();
        assert_eq!(env_default.claude_max_bytes(), 100_000);

        let env_custom = ReadBudgetEnv {
            claude_max_read_tokens: Some(10_000),
            ..Default::default()
        };
        assert_eq!(env_custom.claude_max_bytes(), 40_000);

        let span_bounded = ReadSpan {
            offset: Some(10),
            limit: Some(25),
            ..Default::default()
        };
        assert_eq!(span_bounded.effective_line_count(Some(100)), Some(25));
        assert_eq!(span_bounded.effective_line_count(None), Some(25));

        let span_lines = ReadSpan {
            start_line: Some(5),
            end_line: Some(15),
            ..Default::default()
        };
        assert_eq!(span_lines.effective_line_count(Some(100)), Some(11));
        assert_eq!(span_lines.effective_line_count(None), Some(11));

        let span_unbounded = ReadSpan::default();
        assert_eq!(span_unbounded.effective_line_count(None), None);
    }

    #[test]
    fn test_agent_vendor_serde_forward_compatibility() {
        let opencode_json = "\"opencode\"";
        let parsed: AgentVendor = serde_json::from_str(opencode_json).unwrap();
        assert_eq!(parsed, AgentVendor::OpenCode);
        assert_eq!(parsed.to_string(), "opencode");

        let unknown_json = "\"cursor\"";
        let parsed_unknown: AgentVendor = serde_json::from_str(unknown_json).unwrap();
        assert_eq!(parsed_unknown, AgentVendor::Generic);

        let parsed_from_str: AgentVendor = "opencode".parse().unwrap();
        assert_eq!(parsed_from_str, AgentVendor::OpenCode);
    }

    #[test]
    fn test_read_coverage_display_and_serde() {
        assert_eq!(ReadCoverage::Micro.to_string(), "micro");
        assert_eq!(ReadCoverage::FullFile.to_string(), "full_file");
        assert_eq!(ReadCoverage::PartialMacro.to_string(), "partial_macro");

        let json = serde_json::to_string(&ReadCoverage::FullFile).unwrap();
        assert_eq!(json, "\"full_file\"");
        let de: ReadCoverage = serde_json::from_str(&json).unwrap();
        assert_eq!(de, ReadCoverage::FullFile);
    }
}
