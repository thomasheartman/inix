use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    /// The name of the template to use.
    ///
    /// Inix uses a blank template if you don't specify one.
    #[arg(short, long = "template")]
    template_name: Option<String>,

    /// The directory to initialize.
    ///
    /// Defaults to your current directory if not provided.
    #[arg(short, long = "dir")]
    directory: Option<PathBuf>,

    /// Print a summary of what would be done, but don't do anything.
    #[arg(short = 'n', long = "dry-run", action = clap::ArgAction::SetTrue)]
    dry_run: bool,
}

fn main() {
    let cli: Cli = Cli::parse();

    if let Some(template_name) = cli.template_name.as_deref() {
        println!("You chose this template: {}", template_name);
    }

    if let Some(dir) = cli.directory {
        println!("You chose this dir: {}", dir.display());
    }
}

// fn try_get_template(templatePath: PathBuf): Result<T, E> {

// }
