use common_macros::hash_map;
use std::{
    collections::HashMap,
    env::current_dir,
    fmt::Display,
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context};
use clap::{Parser, ValueEnum};
use indoc::{formatdoc, indoc, writedoc};
use itertools::Itertools;
use rustyline::{error::ReadlineError, Editor};

#[derive(ValueEnum, Clone)]
enum ConflictBehavior {
    Overwrite,
    MergeKeep,
    MergeReplace,
    Cancel,
}

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    /// The name of the template to use.
    ///
    /// Inix uses a blank template if you don't specify one.
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

    /// What to do in case of a pre-existing inix directory where you
    /// are trying to create one. If no value is provided, inix will
    /// prompt you if there is a conflict.
    ///
    /// overwrite: Remove the existing inix directory and create a new one.
    ///
    /// merge-keep: Merge the old and the new directories. If you're
    /// trying to add templates that already exist in the directory,
    /// keep the existing templates instead.
    ///
    /// merge-replace: Merge the old and the new directories. If you're
    /// trying to add templates that already exist in the directory,
    /// remove the old templates and add the new ones.
    ///
    /// cancel: Stop the process without writing any files.
    #[arg(long, value_enum)]
    on_conflict: Option<ConflictBehavior>,
}

fn try_get_target_dir(input: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    match input {
        None => current_dir().context("Failed to read the current working directory."),

        Some(dir) => {
            if dir.is_dir() {
                Ok(dir)
            } else {
                Err(io::Error::from(io::ErrorKind::Other)).with_context(|| {
                    format!(
                        "\"{}\" is not a directory, so I cannot place any files there.",
                        dir.display()
                    )
                })
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum TemplateFiles {
    Nix(&'static str),
    // Envrc(&'static str),
    Both {
        nix: &'static str,
        envrc: &'static str,
    },
}

#[derive(Clone, Debug)]
struct BuiltinTemplate {
    name: &'static str,
    files: TemplateFiles,
    source_dir: PathBuf,
}

#[derive(Clone, Debug)]
enum CustomFiles {
    Nix(String),
    Envrc(String),
    Both { nix: String, envrc: String },
}

#[derive(Clone, Debug)]
struct CustomTemplate<'a> {
    name: &'a str,
    files: CustomFiles,
    source_dir: PathBuf,
}

#[derive(Clone, Debug)]
enum Template<'a> {
    Custom(CustomTemplate<'a>),
    Builtin(BuiltinTemplate),
}

impl<'a> Template<'a> {
    fn name(&self) -> &'a str {
        match self {
            Template::Custom(template) => template.name,
            Template::Builtin(template) => template.name,
        }
    }
}

fn try_get_templates(input_templates: &[String]) -> anyhow::Result<Vec<Template>> {
    #[derive(Clone, Copy, Debug)]
    enum DirErrorReason {
        NotADir,
        NoConfigDir,
        NotFound,
    }

    #[derive(Clone, Debug)]
    struct DirError {
        path: PathBuf,
        reason: DirErrorReason,
    }

    impl Display for DirError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{} ({})", self.path.display(), match self.reason {
                                DirErrorReason::NotADir =>
                                    "which exists, but is not a directory (it's probably a file!)",
                                DirErrorReason::NoConfigDir =>
                                    "but I don't know where your user configuration directory is (this probably means that you're not on Linux, macOS, or Windows)",
                                DirErrorReason::NotFound => "but it doesn't exist",
                            }
)
        }
    }

    // a prioritized list over where to find templates. Items listed earlier take precedence
    let template_locations: Vec<_> = [
        dirs::config_dir()
            .map(|dir| dir.join("inix"))
            .ok_or(DirError {
                path: PathBuf::from("<your user configuration directory>/inix"),
                reason: DirErrorReason::NoConfigDir,
            }),
        // Ok(PathBuf::from("../templates")),
    ]
    .into_iter()
    .map(|result| {
        result.and_then(|dir| {
            if dir.is_dir() {
                Ok(dir)
            } else {
                let reason = match dir.exists() {
                    true => DirErrorReason::NotADir,
                    false => DirErrorReason::NotFound,
                };
                Err(DirError {
                    path: dir.clone(),
                    reason,
                })
            }
        })
    })
    .collect();

    let found_template_dirs: Vec<_> = template_locations
        .iter()
        .filter_map(|x| x.as_deref().map(|y| y.clone()).ok())
        .collect();

    let included_templates = hash_map! {
        "rust" => BuiltinTemplate {
            name: "rust",
            files: TemplateFiles::Nix(include_str!("templates/rust/shell.nix")),
            source_dir: PathBuf::from("inix/templates")
        },
        "node" => BuiltinTemplate {
            name: "node",
            files: TemplateFiles::Both {
                nix: include_str!("templates/node/shell.nix"),
                envrc: include_str!("templates/node/.envrc"),
            },
            source_dir: PathBuf::from("inix/templates")
        },
        "base" => BuiltinTemplate {
            name: "base",
            files: TemplateFiles::Both {
                nix: include_str!("templates/base/shell.nix"),
              envrc: include_str!("templates/base/.envrc"),
            },
            source_dir: PathBuf::from("inix/templates")
        },
    };

    let (oks, errs): (Vec<_>, Vec<_>) = input_templates
        .iter()
        .map(|template_name| {
            found_template_dirs
                .iter()
                .find_map(|location| {
                    let dir = location.join(template_name);
                    match (
                        fs::read_to_string(dir.join("shell.nix")),
                        fs::read_to_string(dir.join(".envrc")),
                    ) {
                        (Err(_), Err(_)) => None,
                        (Ok(nix), Err(_)) => Some(CustomFiles::Nix(nix)),
                        (Err(_), Ok(envrc)) => Some(CustomFiles::Envrc(envrc)),
                        (Ok(nix), Ok(envrc)) => Some(CustomFiles::Both { nix, envrc }),
                    }
                    .map(|files| {
                        Template::Custom(CustomTemplate {
                            name: template_name,
                            source_dir: dir,
                            files,
                        })
                    })
                })
                .or_else(|| {
                    included_templates
                        .get(&template_name as &str)
                        .map(|t| Template::Builtin(t.clone()))
                })
                .ok_or_else(|| anyhow!(template_name.clone()))
        })
        .partition_result();

    if errs.is_empty() {
        Ok(oks)
    } else {
        Err(anyhow!(formatdoc!(
            "
            I couldn't find these templates:
            {}

            I looked (or tried to look) in these places:
            {}",
            errs.iter().map(|name| format!("- {}", name)).join("\n"),
            template_locations
                .iter()
                .map(|location| format!(
                    "- {}",
                    match location {
                        Ok(l) => l.display().to_string(),
                        Err(l) => l.to_string(),
                    }
                ))
                .join("\n"),
        )))
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // PREPARE //

    // check to see whether we can find all the templates
    let templates = try_get_templates(&cli.templates)?;

    // check to see if the target directory exists
    let target_dir = try_get_target_dir(cli.directory)?;

    // check to see whether we have write permissions in the target
    // directory

    let metadata = target_dir.metadata().with_context(|| {
        format!(
            "Unable to read permission status for \"{}\".",
            &target_dir.display()
        )
    })?;

    let false = metadata.permissions().readonly() else {
        bail!(
            "I don't have the right permissions to write to \"{}\"",
            &target_dir.display()
        )
    };

    // does the inix subdirectory already exist?
    let inix_dir = target_dir.join("inix");

    // gather all data about the inix dir
    enum InixDirState<'a> {
        DoesNotExist,
        AlreadyExists { template_collisions: Vec<&'a str> },
    }

    let inix_dir_state = if inix_dir.is_dir() {
        let template_collisions = templates
            .clone()
            .iter()
            .filter_map(|template| {
                if inix_dir.join(template.name()).is_dir() {
                    Some(template.name())
                } else {
                    None
                }
            })
            .collect();

        InixDirState::AlreadyExists {
            template_collisions,
        }
    } else {
        InixDirState::DoesNotExist
    };

    struct Operation<'a> {
        perform_operation: &'a dyn FnOnce() -> anyhow::Result<()>,
        description: String,
    }

    let on_conflict = match (inix_dir_state, cli.on_conflict) {
        (_, Some(behavior)) => behavior,
        (InixDirState::DoesNotExist, None) => ConflictBehavior::Cancel,
        (
            InixDirState::AlreadyExists {
                template_collisions,
            },
            None,
        ) => prompt_for_conflict_behavior(&inix_dir, &template_collisions)?,
    };

    // let file_write_op: anyhow::Result<Operation> = match (&inix_dir_state, cli.on_conflict) {
    //     (InixDirState::DoesNotExist, _) => Ok(Operation {
    //         description: format!(
    //             r#"I will create the `inix` directory "{}" and a subdirectory for each of the provided templates: "{}""#,
    //             inix_dir.display(),
    //             cli.templates.join(", ")
    //         ),
    //         perform_operation: &|| {
    //             let _ = fs::create_dir_all(inix_dir).with_context(|| {
    //                 format!(
    //                     r#"I was unable to create the base inix directory at {}"#,
    //                     inix_dir.display(),
    //                 )
    //             })?;
    //             let template_results = cli.templates.iter().map(|template| {
    //                 let path = inix_dir.join(template);
    //                 let template_source_dir = "template-path";
    //                 fs::copy(template_source_dir, path).with_context(|| {
    //                     format!(
    //                         r#"Failed to copy template "{}" from "{}" to "{}"."#,
    //                         template,
    //                         template_source_dir,
    //                         path.display(),
    //                     )
    //                 })?;
    //                 Ok::<(), anyhow::Error>(())
    //             });
    //             Ok(())
    //         },
    //     }),
    //     (InixDirState::AlreadyExists { .. }, Some(ConflictBehavior::Abort)) => bail!(
    //         r#""{}" already exists. Because you have asked me to abort on conflicts, I cannot proceed any further."#,
    //         inix_dir.display()
    //     ),
    //     (InixDirState::AlreadyExists { .. }, Some(ConflictBehavior::Overwrite)) => todo!(),
    //     (InixDirState::AlreadyExists { .. }, Some(ConflictBehavior::MergeKeep)) => todo!(),
    //     (InixDirState::AlreadyExists { .. }, Some(ConflictBehavior::MergeReplace)) => todo!(),
    //     (InixDirState::AlreadyExists { .. }, None) => todo!("Put user interaction stuff here."),
    // };
    // match inix_dir_state {
    //     InixDirState::DoesNotExist => {}
    //     InixDirState::AlreadyExists {
    //         template_collisions,
    //     } => {
    //         let mut rl = Editor::<()>::new()?;

    //         // Case enumeration

    //         // The inix directory already exists, but none of the new templates conflict with existing subdirectories. Would you like to:
    //         // A: Merge the two inix directories, adding your new templates to the existing directory? (merge_x)
    //         // B: Overwrite the whole directory, removing everything that's in it and replacing it with the new templates? (overwrite)

    //         // The inix directory already exists, and the following templates you're trying to add also exist in the inix directory: <templates>. Would you like to:
    //         // A: Overwrite the entire inix directory, removing anything that exists there already. (overwrite)
    //         // B: Add your templates to the inix directory, overwriting any templates that are there already, but leaving other templates untouched. (merge_replace)
    //         // C: Add your templates to the inix directory, but leaving any templates that exist already (merge_keep)

    //         // The inix directory already exists, and all the templates that you're trying to add also exist already. Would you like to:
    //         // A:overwrite the entire inix directory (overwrite)
    //         // B: only overwrite the subdirectories (merge_replace)
    //         // C: or cancel the operation? (merge_keep)

    //         loop {
    //             println!(
    //                 r#"The directory "{}" already exists. Overwrite completely? (y/N)"#,
    //                 &inix_dir.display()
    //             );
    //             let readline = rl.readline(">> ");
    //             match readline {
    //                 Ok(line) => {
    //                     println!("Line: {}", line)
    //                 }
    //                 Err(ReadlineError::Interrupted) => {
    //                     println!("CTRL-C");
    //                     break;
    //                 }
    //                 Err(ReadlineError::Eof) => {
    //                     println!("CTRL-D");
    //                     break;
    //                 }
    //                 Err(err) => {
    //                     println!("Error: {:?}", err);
    //                     break;
    //                 }
    //             }
    //         }
    //     }
    // }

    // If so, what about subdirectories? What do you want to do in case of conflicts?

    // get templates

    // EXECUTE //

    // copy templates over (into an inix directory)

    //
    Ok(())
}

fn prompt_for_conflict_behavior(
    inix_dir: &Path,
    conflicting_templates: &[&str],
) -> anyhow::Result<ConflictBehavior> {
    let mut rl = Editor::<()>::new()?;

    #[derive(Debug, Clone, Copy)]
    struct PromptOption {
        description: &'static str,
        short_description: &'static str,
    }

    impl Display for PromptOption {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, r#"{} ({})"#, self.description, self.short_description)
        }
    }

    #[derive(Debug, Clone)]
    struct Prompt {
        text: String,
        options: HashMap<char, PromptOption>,
    }

    impl Prompt {
        fn list_options(&self) -> String {
            self.options
                .iter()
                .sorted_by_key(|(key, _)| *key)
                .map(|(key, prompt_option)| format!("- {}: {}", key, prompt_option.to_string()))
                .join("\n")
        }

        fn list_option_keys(&self) -> String {
            let sorted_keys = self.options.keys().sorted();

            let quote = |c| format!(r#""{}""#, c);

            match self.options.len() {
                2 => sorted_keys.map(quote).join(" and "),
                len => {
                    let add_and = if len > 2 {
                        self.options.keys().max()
                    } else {
                        None
                    };

                    sorted_keys
                        .map(|key| match add_and {
                            Some(char) if key == char => format!(r#"and {}"#, quote(key)),
                            _ => quote(key),
                        })
                        .join(", ")
                }
            }
        }
    }

    impl Display for Prompt {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            writedoc!(
                f,
                r#"
                {}

                How would you like to proceed?
                  {}

                Please enter exactly one option (one of {} [case-insensitive])."#,
                self.text,
                self.list_options(),
                self.list_option_keys()
            )
        }
    }

    let text = match conflicting_templates {
        [] => Prompt {
            options: hash_map! {
                'A' => PromptOption {description:  "Merge the two inix directories, adding your new templates to the existing directory?", short_description: "merge" },
                'B' => PromptOption{ description:  "Overwrite the whole directory, removing everything that's in it and replacing it with the new templates?", short_description: "overwrite" },
                'C' => PromptOption { description: "Cancel the operation", short_description: "cancel" }
            },
            text: formatdoc!(
                r#"
            The inix directory ("{}") already exists, but none of the new templates conflict with existing subdirectories. Would you like to:

            Please enter the option's letter."#,
                inix_dir.display()
            ),
        },
        _ => todo!(),
    };

    // Case enumeration

    // The inix directory already exists, and the following templates you're trying to add also exist in the inix directory: <templates>. Would you like to:
    // A: Overwrite the entire inix directory, removing anything that exists there already. (overwrite)
    // B: Add your templates to the inix directory, overwriting any templates that are there already, but leaving other templates untouched. (merge_replace)
    // C: Add your templates to the inix directory, but leaving any templates that exist already (merge_keep)

    // The inix directory already exists, and all the templates that you're trying to add also exist already. Would you like to:
    // A:overwrite the entire inix directory (overwrite)
    // B: only overwrite the subdirectories (merge_replace)
    // C: or cancel the operation? (merge_keep)

    loop {
        println!(
            r#"The directory "{}" already exists. Overwrite completely? (y/N)"#,
            &inix_dir.display()
        );
        let readline = rl.readline(">> ");
        match readline {
            Ok(line) => {
                println!("Line: {}", line)
            }
            Err(ReadlineError::Interrupted) => {
                println!("CTRL-C");
                break;
            }
            Err(ReadlineError::Eof) => {
                println!("CTRL-D");
                break;
            }
            Err(err) => {
                println!("Error: {:?}", err);
                break;
            }
        }
    }

    todo!()
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
