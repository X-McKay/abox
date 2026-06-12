pub mod attach;
pub mod audit;
pub mod ca;
pub mod capabilities;
pub mod divergence;
pub mod doctor;
pub mod env;
pub mod grant;
pub mod init;
pub mod list;
pub mod merge;
pub mod project;
pub mod run;
pub mod snapshot;
pub mod stop;
pub mod template;

use anyhow::Result;
use std::path::Path;

pub(crate) fn validate_task_arg(task: &str) -> Result<()> {
    abox_core::util::validate_task_id(task)
        .map_err(|e| anyhow::anyhow!("invalid task ID {task:?}: {e}"))
}

pub(crate) fn validate_task_arg_for_runtime_dir(task: &str, runtime_dir: &Path) -> Result<()> {
    abox_core::util::validate_task_id_for_runtime_dir(task, runtime_dir)
        .map_err(|e| anyhow::anyhow!("invalid task ID {task:?}: {e}"))
}
