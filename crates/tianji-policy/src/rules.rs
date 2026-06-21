//! "Always allow" rules (DESIGN.md §4.3). Workspace-scoped by default; promotion to global is
//! an explicit action handled at the store layer (copy rule from WorkspaceStore → AppStore).

use serde::{Deserialize, Serialize};
use tianji_types::Target;

/// How broadly an allow-rule matches. The middle variant is the recommended default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowGranularity {
    /// Exactly this argv.
    ExactCommand,
    /// This tool + this flag shape against any in-scope target. (default)
    ToolFlagShape,
    /// This whole tool, any args. Power-user; riskier.
    WholeTool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowRule {
    pub tool: String,
    pub granularity: AllowGranularity,
    /// For `ExactCommand`/`ToolFlagShape`: the flag/argv fingerprint to match.
    pub fingerprint: Vec<String>,
}

impl AllowRule {
    pub fn matches(&self, tool: &str, argv: &[String], _targets: &[Target]) -> bool {
        if self.tool != tool {
            return false;
        }
        match self.granularity {
            AllowGranularity::WholeTool => true,
            AllowGranularity::ExactCommand => self.fingerprint == argv,
            AllowGranularity::ToolFlagShape => {
                let flags: Vec<&String> = argv.iter().filter(|a| a.starts_with('-')).collect();
                let want: Vec<&String> = self.fingerprint.iter().filter(|a| a.starts_with('-')).collect();
                flags == want
            }
        }
    }
}
