//! `abox --capabilities` — print the Phase 0 capability envelope.
//!
//! This handler must not load config, policy, or runtime dirs.

use anyhow::Result;

/// Phase 0 envelope shape — frozen by the integration contract.
pub fn execute() -> Result<()> {
    let json = concat!(
        "{",
        "\"protocolVersions\":[1,3],",
        "\"taskKinds\":[\"assistant_job\",\"explicit_command\",\"verification_check\"],",
        "\"executionEngines\":[\"agent_cli\",\"shell\"]",
        "}",
    );

    println!("{json}");
    Ok(())
}
