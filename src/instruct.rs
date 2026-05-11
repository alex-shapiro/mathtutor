//! `mt instruct`: print the agent operator playbook baked into the
//! binary at compile time.

use crate::Result;

const PLAYBOOK: &str = include_str!("instruct.md");

pub fn cmd_instruct() -> Result<()> {
    print!("{PLAYBOOK}");
    Ok(())
}
