pub mod claude;
pub mod codex;
pub mod opencode;

mod gemini;
mod grok;
mod pi;
mod prince;

use crate::error::AppError;
use crate::model::{AppKind, UsageEntry};

pub fn collect(apps: &[AppKind]) -> Result<Vec<UsageEntry>, AppError> {
    let mut entries = Vec::new();
    for &app in apps {
        match app {
            AppKind::Claude => entries.extend(claude::collect()?),
            AppKind::Codex => entries.extend(codex::collect()?),
            AppKind::OpenCode => entries.extend(opencode::collect()?),
        }
    }
    Ok(entries)
}
