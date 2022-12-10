use std::{
    env::current_dir,
    fmt::Display,
    path::{Path, PathBuf},
};

use anyhow::Context;
use clap::Parser;

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    /// The name of the template to use.
    ///
    /// Inix uses a blank template if you don't specify one.
    #[arg(short, long)]
    templates: Vec<String>,

    /// The directory to initialize.
    ///
    /// Defaults to your current directory if not provided.
    #[arg(short, long)]
    directory: Option<PathBuf>,

    /// Print a summary of what would be done, but don't do anything.
    #[arg(short = 'n', long, action = clap::ArgAction::SetTrue)]
    dry_run: bool,

    /// Whether inix should run `direnv allow` for you or not.
    /// Defaults to false.
    ///
    /// You should only set this to true if you trust the templates
    /// you use for instantiation.
    #[arg(short, long, action = clap::ArgAction::SetTrue)]
    auto_allow: bool,
}

fn try_get_target_dir(input: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    match input {
        None => current_dir().context("Failed to read the current working directory."),

        Some(dir) => {
            let data = std::fs::metadata(AsRef::<Path>::as_ref(&dir)).with_context(|| {
                format!(
                    "Failed to read metadata about \"{}\". It probably does not exist.",
                    dir.display()
                )
            })?;
            if std::fs::Metadata::is_dir(&data) {
                Ok(dir)
            } else {
                Err(std::io::Error::from(std::io::ErrorKind::Other)).with_context(|| {
                    format!(
                        "\"{}\" is not a directory, so I cannot place any files there.",
                        dir.display()
                    )
                })
            }
        }
    }
}

fn execute(cli: Cli) -> anyhow::Result<()> {
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // PREPARE //

    // check to see if the target directory exists
    try_get_target_dir(cli.directory)?;

    // check to see whether we have write permissions in the target
    // directory

    // does the inix subdirectory already exist?
    // If so, what about subdirectories? What do you want to do in case of conflicts?

    // get templates

    // EXECUTE //

    // copy templates over (into an inix directory)

    //

    execute(cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Verify that the CLI is configured correctly.
    fn verify_cli() {
        use clap::CommandFactory;
        Cli::command().debug_assert()
    }
}
