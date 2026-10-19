use crate::Result;

const PLAYBOOK: &str = include_str!("instruct.md");

pub fn cmd_instruct() -> Result<()> {
    print!("{PLAYBOOK}");
    Ok(())
}
