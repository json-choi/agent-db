//! Runtime-independent shell completion generation.

use std::io;

use clap::CommandFactory;
use clap_complete::{generate, Shell};

use crate::args::Cli;
use crate::client::ClientError;

pub(crate) fn write(shell: Shell) -> Result<(), ClientError> {
    let mut command = Cli::command();
    generate(shell, &mut command, "dopedb", &mut io::stdout());
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use clap_complete::{generate, Shell};

    use crate::args::Cli;

    #[test]
    fn generated_scripts_name_the_installed_binary_and_never_embed_session_material() {
        for shell in [
            Shell::Bash,
            Shell::Elvish,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Zsh,
        ] {
            let mut output = Vec::new();
            generate(shell, &mut Cli::command(), "dopedb", &mut output);
            let script = String::from_utf8(output).unwrap();
            assert!(script.contains("dopedb"));
            assert!(!script.contains("DOPEDB_SESSION_TOKEN"));
            assert!(!script.contains("DOPEDB_RUNTIME_FILE"));
        }
    }
}
