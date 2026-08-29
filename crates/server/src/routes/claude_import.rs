//! Import of existing Claude Code CLI chat history.
//!
//! The Claude Code CLI stores transcripts as JSONL under
//! `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`. cdesktop has no notion of
//! them: `crates/review` reads that store for the standalone review CLI, but neither
//! `server` nor `tauri-app` depends on `review`, so nothing in the desktop app ever
//! surfaced them.
//!
//! This module discovers those transcripts and materialises them into cdesktop's own
//! storage (`repos` -> `workspaces` -> `sessions` -> `execution_processes` ->
//! `coding_agent_turns`, plus an execution log file). Imported sessions are therefore
//! ordinary cdesktop sessions and render in the existing sidebar and transcript viewer
//! with no frontend changes.
//!
//! `~/.claude` is treated as strictly read-only: transcripts are parsed, never written,
//! moved, or reformatted.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use axum::{
    Router,
    extract::State,
    response::Json as ResponseJson,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use db::models::{
    coding_agent_turn::{CodingAgentTurn, CreateCodingAgentTurn},
    execution_process::{
        CreateExecutionProcess, ExecutionProcess, ExecutionProcessRunReason,
        ExecutionProcessStatus,
    },
    session::{CreateSession, Session},
    workspace::{CreateWorkspace, Workspace},
    workspace_repo::{CreateWorkspaceRepo, WorkspaceRepo},
};
use deployment::Deployment;
use executors::{
    actions::{ExecutorAction, ExecutorActionType, coding_agent_initial::CodingAgentInitialRequest},
    logs::{
        ActionType, NormalizedEntry, NormalizedEntryType, ToolStatus,
        utils::patch::ConversationPatch,
    },
    profile::ExecutorConfig,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;
use utils::{execution_logs::ExecutionLogWriter, log_msg::LogMsg, response::ApiResponse};
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError};

/// A Claude Code session recovered from disk, already normalised for cdesktop.
#[derive(Debug, Clone, Serialize, TS)]
pub struct DiscoveredSession {
    /// Claude's own session UUID. Doubles as the idempotency key: it is stored on
    /// `coding_agent_turns.agent_session_id` so re-running the import skips it.
    pub claude_session_id: String,
    /// Real working directory, taken from the records themselves rather than decoded
    /// from the directory name. That encoding is lossy (both path separators and
    /// underscores become `-`, so `Bounty_Engine` and `Bounty-Engine` collide).
    pub cwd: String,
    pub title: String,
    pub message_count: usize,
    pub started_at: Option<DateTime<Utc>>,
    pub git_branch: Option<String>,
    #[ts(skip)]
    #[serde(skip)]
    pub entries: Vec<NormalizedEntry>,
    #[ts(skip)]
    #[serde(skip)]
    pub first_prompt: Option<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct ScanResponse {
    pub sessions: Vec<DiscoveredSession>,
    pub already_imported: usize,
}

#[derive(Debug, Deserialize, TS)]
pub struct ImportRequest {
    /// Restrict the import to these Claude session ids. `None` imports everything found.
    #[serde(default)]
    pub session_ids: Option<Vec<String>>,
}

#[derive(Debug, Serialize, TS)]
pub struct ImportResponse {
    pub imported: usize,
    pub skipped: usize,
    pub failed: Vec<String>,
}

fn claude_projects_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".claude").join("projects"))
}

/// Transcripts record the working directory with whatever drive-letter case the
/// shell happened to use, so the same project shows up as both `c:\...` and
/// `C:\...`. Repo registration is case-sensitive, so without this the same
/// directory registers twice.
fn normalize_cwd(cwd: &str) -> String {
    let mut chars = cwd.chars();
    match (chars.next(), chars.next()) {
        (Some(drive), Some(':')) if drive.is_ascii_alphabetic() => {
            format!("{}:{}", drive.to_ascii_uppercase(), chars.as_str())
        }
        _ => cwd.to_string(),
    }
}

/// Pull the text out of a content block array, ignoring non-text blocks.
fn text_of(content: &serde_json::Value) -> Option<String> {
    let arr = content.as_array()?;
    let mut out = String::new();
    for block in arr {
        if block.get("type").and_then(|t| t.as_str()) == Some("text")
            && let Some(t) = block.get("text").and_then(|t| t.as_str())
        {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(t);
        }
    }
    if out.trim().is_empty() { None } else { Some(out) }
}

fn message_entry_type(role: &str) -> NormalizedEntryType {
    if role == "assistant" {
        NormalizedEntryType::AssistantMessage
    } else {
        NormalizedEntryType::UserMessage
    }
}

/// Convert one content block into a normalized entry.
fn block_to_entry(
    block: &serde_json::Value,
    role: &str,
    timestamp: Option<String>,
) -> Option<NormalizedEntry> {
    let kind = block.get("type").and_then(|t| t.as_str())?;
    match kind {
        "text" => {
            let text = block.get("text").and_then(|t| t.as_str())?;
            if text.trim().is_empty() {
                return None;
            }
            Some(NormalizedEntry {
                timestamp,
                entry_type: message_entry_type(role),
                content: text.to_string(),
                metadata: None,
            })
        }
        "thinking" => {
            let thinking = block.get("thinking").and_then(|t| t.as_str())?;
            if thinking.trim().is_empty() {
                return None;
            }
            Some(NormalizedEntry {
                timestamp,
                entry_type: NormalizedEntryType::Thinking,
                content: thinking.to_string(),
                metadata: None,
            })
        }
        "tool_use" => {
            let name = block
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("tool")
                .to_string();
            let arguments = block.get("input").cloned();
            // Historical transcripts are finished, so every recorded call already ran.
            // The paired tool_result is not replayed: NormalizedEntry has no ToolResult
            // variant, which is why the live normaliser drops those too.
            Some(NormalizedEntry {
                timestamp,
                entry_type: NormalizedEntryType::ToolUse {
                    tool_name: name.clone(),
                    action_type: ActionType::Tool {
                        tool_name: name,
                        arguments,
                        result: None,
                    },
                    status: ToolStatus::Success,
                },
                content: String::new(),
                metadata: None,
            })
        }
        _ => None,
    }
}

/// Parse a single `<session-id>.jsonl` transcript.
fn parse_transcript(path: &Path) -> Option<DiscoveredSession> {
    let raw = std::fs::read_to_string(path).ok()?;

    let mut cwd: Option<String> = None;
    let mut title: Option<String> = None;
    let mut git_branch: Option<String> = None;
    let mut started_at: Option<DateTime<Utc>> = None;
    let mut first_prompt: Option<String> = None;
    let mut entries: Vec<NormalizedEntry> = Vec::new();
    let mut claude_session_id: Option<String> = None;

    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let rec_type = rec.get("type").and_then(|t| t.as_str()).unwrap_or("");

        if claude_session_id.is_none()
            && let Some(sid) = rec.get("sessionId").and_then(|s| s.as_str())
        {
            claude_session_id = Some(sid.to_string());
        }

        match rec_type {
            // Claude's own generated title: the nicest label for the sidebar.
            "ai-title" => {
                if let Some(t) = rec.get("aiTitle").and_then(|t| t.as_str())
                    && !t.trim().is_empty()
                {
                    title = Some(t.to_string());
                }
            }
            "user" | "assistant" => {
                if cwd.is_none()
                    && let Some(c) = rec.get("cwd").and_then(|c| c.as_str())
                {
                    cwd = Some(c.to_string());
                }
                if git_branch.is_none()
                    && let Some(b) = rec.get("gitBranch").and_then(|b| b.as_str())
                    && !b.is_empty()
                {
                    git_branch = Some(b.to_string());
                }

                let timestamp = rec
                    .get("timestamp")
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string());
                if started_at.is_none()
                    && let Some(ts) = timestamp.as_deref()
                    && let Ok(parsed) = DateTime::parse_from_rfc3339(ts)
                {
                    started_at = Some(parsed.with_timezone(&Utc));
                }

                let Some(message) = rec.get("message") else {
                    continue;
                };
                let role = message
                    .get("role")
                    .and_then(|r| r.as_str())
                    .unwrap_or(rec_type);
                let Some(content) = message.get("content") else {
                    continue;
                };

                match content {
                    serde_json::Value::Array(blocks) => {
                        if role == "user" && first_prompt.is_none() {
                            first_prompt = text_of(content);
                        }
                        for block in blocks {
                            if let Some(entry) = block_to_entry(block, role, timestamp.clone()) {
                                entries.push(entry);
                            }
                        }
                    }
                    // Older transcripts store message content as a bare string.
                    serde_json::Value::String(s) if !s.trim().is_empty() => {
                        entries.push(NormalizedEntry {
                            timestamp: timestamp.clone(),
                            entry_type: message_entry_type(role),
                            content: s.clone(),
                            metadata: None,
                        });
                        if role == "user" && first_prompt.is_none() {
                            first_prompt = Some(s.clone());
                        }
                    }
                    _ => {}
                }
            }
            // Everything else (attachment, file-history-*, bridge-session,
            // queue-operation, atis-latch, mode, last-prompt) carries no
            // conversation content worth rendering.
            _ => {}
        }
    }

    let claude_session_id = claude_session_id
        .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(String::from))?;
    let cwd = cwd?;
    if entries.is_empty() {
        return None;
    }

    let title = title
        .or_else(|| {
            first_prompt.as_ref().map(|p| {
                let truncated: String = p.chars().take(60).collect();
                truncated.replace('\n', " ").trim().to_string()
            })
        })
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Imported session".to_string());

    Some(DiscoveredSession {
        claude_session_id,
        cwd: normalize_cwd(&cwd),
        title,
        message_count: entries.len(),
        started_at,
        git_branch,
        entries,
        first_prompt,
    })
}

/// Walk `~/.claude/projects` and parse every transcript found.
fn scan_disk() -> Vec<DiscoveredSession> {
    let Some(root) = claude_projects_dir() else {
        return Vec::new();
    };
    let Ok(project_dirs) = std::fs::read_dir(&root) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for project in project_dirs.flatten() {
        if !project.path().is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(session) = parse_transcript(&path) {
                out.push(session);
            }
        }
    }
    // Newest first, matching the sidebar's ordering.
    out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    out
}

/// Claude session ids already present in the database.
async fn already_imported_ids(deployment: &DeploymentImpl) -> Result<HashSet<String>, sqlx::Error> {
    // Deliberately not a `query!` macro: this is a new query, and the macro form would
    // require regenerating the offline sqlx metadata.
    let rows: Vec<(Option<String>,)> =
        sqlx::query_as("SELECT agent_session_id FROM coding_agent_turns")
            .fetch_all(&deployment.db().pool)
            .await?;
    Ok(rows.into_iter().filter_map(|(id,)| id).collect())
}

async fn scan(
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<ScanResponse>>, ApiError> {
    let existing = already_imported_ids(&deployment).await?;
    let all = scan_disk();
    let total = all.len();
    let sessions: Vec<_> = all
        .into_iter()
        .filter(|s| !existing.contains(&s.claude_session_id))
        .collect();
    let already_imported = total - sessions.len();

    Ok(ResponseJson(ApiResponse::success(ScanResponse {
        sessions,
        already_imported,
    })))
}

/// Materialise one discovered session into cdesktop's storage.
async fn import_one(
    deployment: &DeploymentImpl,
    discovered: &DiscoveredSession,
) -> Result<(), ApiError> {
    let pool = &deployment.db().pool;

    // 1. Repo for the transcript's working directory. Registration is idempotent.
    let repo = deployment
        .repo()
        .register(pool, discovered.cwd.as_str(), None)
        .await
        .map_err(|e| ApiError::BadRequest(format!("register repo {}: {e}", discovered.cwd)))?;

    // 2. Workspace.
    let branch = discovered
        .git_branch
        .clone()
        .unwrap_or_else(|| "main".to_string());
    let workspace = Workspace::create(
        pool,
        &CreateWorkspace {
            branch: branch.clone(),
            name: Some(discovered.title.clone()),
            // Imported history records work already done; there is no worktree for it.
            use_worktree: false,
        },
        Uuid::new_v4(),
    )
    .await?;

    WorkspaceRepo::create_many(
        pool,
        workspace.id,
        &[CreateWorkspaceRepo {
            repo_id: repo.id,
            target_branch: branch,
        }],
    )
    .await?;

    // 3. Session.
    let session = Session::create(
        pool,
        &CreateSession {
            executor: Some("CLAUDE_CODE".to_string()),
            name: Some(discovered.title.clone()),
        },
        Uuid::new_v4(),
        workspace.id,
    )
    .await?;

    // 4. Execution process, recorded as an already-finished coding agent run.
    // Built through serde so the many optional, `#[serde(default)]` fields on
    // ExecutorConfig do not have to be spelled out here.
    let executor_config: ExecutorConfig =
        serde_json::from_value(json!({ "executor": "CLAUDE_CODE" }))
            .map_err(|e| ApiError::BadRequest(format!("executor config: {e}")))?;

    let executor_action = ExecutorAction::new(
        ExecutorActionType::CodingAgentInitialRequest(CodingAgentInitialRequest {
            prompt: discovered.first_prompt.clone().unwrap_or_default(),
            executor_config,
            working_dir: None,
        }),
        None,
    );

    let process = ExecutionProcess::create(
        pool,
        &CreateExecutionProcess {
            session_id: session.id,
            executor_action,
            run_reason: ExecutionProcessRunReason::CodingAgent,
        },
        Uuid::new_v4(),
        &[],
    )
    .await?;

    // 5. Transcript, written in the same JSONL-of-LogMsg form the live path uses.
    let mut writer = ExecutionLogWriter::new_for_execution(session.id, process.id).await?;

    for (index, entry) in discovered.entries.iter().enumerate() {
        let patch = ConversationPatch::add_normalized_entry(index, entry.clone());
        let line = serde_json::to_string(&LogMsg::JsonPatch(patch))
            .map_err(std::io::Error::other)?;
        writer.append_jsonl_line(&format!("{line}\n")).await?;
    }

    let finished = serde_json::to_string(&LogMsg::Finished).map_err(std::io::Error::other)?;
    writer.append_jsonl_line(&format!("{finished}\n")).await?;

    ExecutionProcess::update_completion(
        pool,
        process.id,
        ExecutionProcessStatus::Completed,
        Some(0),
    )
    .await?;

    // 6. Turn. `agent_session_id` carries Claude's own session id, which makes the
    //    import idempotent and preserves the link back to the real transcript.
    let turn = CodingAgentTurn::create(
        pool,
        &CreateCodingAgentTurn {
            execution_process_id: process.id,
            prompt: discovered.first_prompt.clone(),
        },
        Uuid::new_v4(),
    )
    .await?;
    // Both of these key on execution_process_id, not the turn id.
    let _ = turn;
    CodingAgentTurn::update_agent_session_id(pool, process.id, &discovered.claude_session_id)
        .await?;
    CodingAgentTurn::update_summary(pool, process.id, &discovered.title).await?;

    Ok(())
}

async fn run_import(
    State(deployment): State<DeploymentImpl>,
    axum::Json(payload): axum::Json<ImportRequest>,
) -> Result<ResponseJson<ApiResponse<ImportResponse>>, ApiError> {
    let existing = already_imported_ids(&deployment).await?;
    let wanted: Option<HashSet<String>> =
        payload.session_ids.map(|ids| ids.into_iter().collect());

    let mut imported = 0usize;
    let mut skipped = 0usize;
    let mut failed = Vec::new();

    for discovered in scan_disk() {
        if existing.contains(&discovered.claude_session_id) {
            skipped += 1;
            continue;
        }
        if let Some(wanted) = &wanted
            && !wanted.contains(&discovered.claude_session_id)
        {
            skipped += 1;
            continue;
        }

        match import_one(&deployment, &discovered).await {
            Ok(()) => imported += 1,
            Err(e) => {
                tracing::warn!(
                    "Failed to import Claude session {}: {e}",
                    discovered.claude_session_id
                );
                failed.push(discovered.claude_session_id.clone());
            }
        }
    }

    Ok(ResponseJson(ApiResponse::success(ImportResponse {
        imported,
        skipped,
        failed,
    })))
}

pub fn router(_deployment: &DeploymentImpl) -> Router<DeploymentImpl> {
    Router::new().nest(
        "/claude-import",
        Router::new()
            .route("/scan", get(scan))
            .route("/run", post(run_import)),
    )
}
