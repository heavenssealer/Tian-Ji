//! Approval resolution + allow-rule management.

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::state::{AppError, AppResult, AppState};
use tianji_agent::{ApprovalOutcome, ApprovalToken};
use tianji_policy::{AllowGranularity, AllowRule};
use tianji_types::uuid::Uuid;

/// Mirror of the approval card's actions for the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalResolution {
    ApproveOnce,
    ApproveEdited { argv: Vec<String> },
    Deny { reason: String },
    AlwaysAllow { granularity: String },
}

/// A serialized allow-rule returned by `policy_rules_list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRuleDto {
    pub tool: String,
    pub granularity: String,
    pub fingerprint: Vec<String>,
    pub scope: String,
    /// The raw JSON used to identify the rule for deletion.
    pub rule_json: String,
}

fn parse_granularity(s: &str) -> AppResult<AllowGranularity> {
    Ok(match s {
        "exact_command" => AllowGranularity::ExactCommand,
        "tool_flag_shape" => AllowGranularity::ToolFlagShape,
        "whole_tool" => AllowGranularity::WholeTool,
        other => return Err(AppError::Message(format!("unknown granularity: {other}"))),
    })
}

fn granularity_str(g: &AllowGranularity) -> &'static str {
    match g {
        AllowGranularity::ExactCommand  => "exact_command",
        AllowGranularity::ToolFlagShape => "tool_flag_shape",
        AllowGranularity::WholeTool     => "whole_tool",
    }
}

fn rule_to_dto(rule: &AllowRule, scope: &str) -> PolicyRuleDto {
    PolicyRuleDto {
        tool: rule.tool.clone(),
        granularity: granularity_str(&rule.granularity).to_string(),
        fingerprint: rule.fingerprint.clone(),
        scope: scope.to_string(),
        rule_json: serde_json::to_string(rule).unwrap_or_default(),
    }
}

#[tauri::command]
pub async fn policy_resolve(
    state: State<'_, AppState>,
    token: String,
    resolution: ApprovalResolution,
) -> AppResult<()> {
    let cw = state.current()?;
    let gate = cw.orchestrator.gate();
    let token = ApprovalToken(Uuid::parse_str(&token).map_err(|e| AppError::Message(e.to_string()))?);

    let outcome = match resolution {
        ApprovalResolution::ApproveOnce => ApprovalOutcome::ApproveOnce,
        ApprovalResolution::ApproveEdited { argv } => ApprovalOutcome::ApproveEdited(argv),
        ApprovalResolution::Deny { reason } => ApprovalOutcome::Deny(reason),
        ApprovalResolution::AlwaysAllow { granularity } => {
            let call = gate
                .call_for(token)
                .ok_or_else(|| AppError::Message("approval token expired".into()))?;
            let rule = AllowRule {
                tool: call.tool,
                granularity: parse_granularity(&granularity)?,
                fingerprint: call.argv,
            };
            ApprovalOutcome::AlwaysAllow(rule)
        }
    };

    gate.resolve(token, outcome);
    Ok(())
}

#[tauri::command]
pub async fn policy_rules_list(state: State<'_, AppState>) -> AppResult<Vec<PolicyRuleDto>> {
    let cw = state.current()?;
    let mut out: Vec<PolicyRuleDto> = cw
        .store
        .allow_rules()?
        .iter()
        .map(|r| rule_to_dto(r, "workspace"))
        .collect();
    out.extend(
        state
            .app
            .global_rules()?
            .iter()
            .map(|r| rule_to_dto(r, "global")),
    );
    Ok(out)
}

#[tauri::command]
pub async fn policy_rule_remove(
    state: State<'_, AppState>,
    rule_json: String,
    is_global: bool,
) -> AppResult<()> {
    let cw = state.current()?;
    if is_global {
        state.app.remove_global_rule(&rule_json)?;
    } else {
        cw.store.remove_allow_rule(&rule_json)?;
    }
    Ok(())
}
