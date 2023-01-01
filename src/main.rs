use common_macros::hash_map;
use handlebars::Handlebars;
use nonempty::NonEmpty;
use std::{
    collections::HashMap,
    env::current_dir,
    fmt::Display,
    fs::{self, create_dir_all, remove_dir_all},
    io,
    path::PathBuf,
};

use anyhow::{anyhow, bail, Context};
use clap::{Parser, ValueEnum};
use indoc::{formatdoc, writedoc};
use itertools::Itertools;
use rustyline::{error::ReadlineError, Editor};

#[derive(ValueEnum, Clone, Copy, Debug)]
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

    fn path(&self) -> PathBuf {
        match self {
            Template::Custom(template) => template.source_dir.join(template.name),
            Template::Builtin(template) => template.source_dir.join(template.name),
        }
    }

    fn files(&self) -> Vec<(&'static str, &'a str)> {
        match &self {
            Template::Custom(t) => match &t.files {
                CustomFiles::Nix(content) => vec![("shell.nix", &content)],
                CustomFiles::Envrc(content) => vec![(".envrc", &content)],
                CustomFiles::Both { nix, envrc } => {
                    vec![(".envrc", &envrc), ("shell.nix", &nix)]
                }
            },
            Template::Builtin(t) => match &t.files {
                TemplateFiles::Nix(content) => vec![("shell.nix", &content)],
                TemplateFiles::Both { nix, envrc } => {
                    vec![(".envrc", &envrc), ("shell.nix", &nix)]
                }
            },
        }
    }
}

fn included_templates() -> HashMap<&'static str, BuiltinTemplate> {
    hash_map! {
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
                nix: include_str!("templates/base/shell.nix.template"),
              envrc: include_str!("templates/base/.envrc.template"),
            },
            source_dir: PathBuf::from("inix/templates")
        },
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
                    included_templates()
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

#[derive(Clone, Debug)]
enum TemplateCollisions<'a> {
    None,
    All(NonEmpty<&'a str>),
    Some(NonEmpty<&'a str>),
}

#[derive(Debug, Clone)]
enum InixDirState<'a> {
    DoesNotExist,
    AlreadyExists {
        template_collisions: TemplateCollisions<'a>,
    },
}

#[derive(Debug, Clone)]
struct InixDir<'a> {
    path: &'a PathBuf,
    state: InixDirState<'a>,
}

impl<'a> InixDir<'a> {
    fn conflict_description(&self) -> String {
        match &self.state {
            InixDirState::DoesNotExist => format!(
                r#"The inix directory ({}) does not exist."#,
                self.path.display()
            ),
            InixDirState::AlreadyExists {
                template_collisions,
            } => match template_collisions {
                TemplateCollisions::None => format!(
                    r#"
            The inix directory ("{}") already exists, but none of the new templates conflict with existing subdirectories."#,
                    self.path.display()
                ),
                TemplateCollisions::All(conflicts) => format!(
                    r#"The inix directory ("{}") already exists, and it contains all of the templates that you're trying to add ({})."#,
                    self.path.display(),
                    combine_strings(conflicts.into_iter())
                ),
                TemplateCollisions::Some(conflicts) => format!(
                    r#"The inix directory ("{}") already exists, and the following templates you're trying to add already exist in the inix directory: {}."#,
                    self.path.display(),
                    combine_strings(conflicts.into_iter())
                ),
            },
        }
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
    let inix_dir_path = target_dir.join("inix");

    // gather all data about the inix dir

    let inix_dir = {
        let state = if inix_dir_path.is_dir() {
            let conflicting_templates: Vec<&str> = templates
                .clone()
                .iter()
                .filter_map(|template| {
                    if inix_dir_path.join(template.name()).is_dir() {
                        Some(template.name())
                    } else {
                        None
                    }
                })
                .collect();

            let template_collisions = match conflicting_templates.as_slice() {
                [] => TemplateCollisions::None,
                [head, tail @ ..] if conflicting_templates.len() == templates.len() => {
                    TemplateCollisions::All(NonEmpty::from((*head, tail.iter().copied().collect())))
                }
                [head, tail @ ..] => TemplateCollisions::Some(NonEmpty::from((
                    *head,
                    tail.iter().copied().collect(),
                ))),
            };

            InixDirState::AlreadyExists {
                template_collisions,
            }
        } else {
            InixDirState::DoesNotExist
        };

        InixDir {
            state,
            path: &inix_dir_path,
        }
    };

    let on_conflict = match (&inix_dir.state, cli.on_conflict) {
        (_, Some(behavior)) => behavior,
        (InixDirState::DoesNotExist, None) => ConflictBehavior::Cancel,
        (InixDirState::AlreadyExists { .. }, None) => prompt_for_conflict_behavior(&inix_dir)?,
    };

    // EXECUTE //
    if cli.dry_run {
        println!("So here's the plan:");
        match inix_dir.state {
            InixDirState::DoesNotExist => {
                println!(
                    r#"I will create the "{}" directory."#,
                    inix_dir.path.display()
                );
                println!(
                    r#"I will then add the {} template(s) to that directory."#,
                    combine_strings(templates.iter().map(|t| t.name()))
                );
                let conflict_behavior = match on_conflict {
                    ConflictBehavior::Overwrite => "completely overwrite the existing directory",
                    ConflictBehavior::MergeKeep => {
                        "merge the two directories, keeping existing files on collisions"
                    }
                    ConflictBehavior::MergeReplace => {
                        "merge the two directories, replacing existing files on collisions"
                    }
                    ConflictBehavior::Cancel => "cancel the operation and exit",
                };
                println!(
                    r#"If the directory were to be created in the meantime, I would "{}"."#,
                    conflict_behavior
                );
            }
            InixDirState::AlreadyExists {
                ref template_collisions,
            } => {
                println!("{}", inix_dir.conflict_description());

                let new_template_names = templates.iter().map(Template::name);

                let msg =
                    // overwrite
                match (on_conflict, template_collisions) {
                    (ConflictBehavior::Overwrite, _) => format!(r#"Because you have chosen to overwrite the inix directory on conflicts, I will delete the existing directory ("{}") and recreate it with the templates you have chosen ({})."#, inix_dir.path.display(), combine_strings(new_template_names)),

                    // merge (keep)
                    (ConflictBehavior::MergeKeep, TemplateCollisions::Some(ts) ) => {
                        format!(r#"Because you have chosen the merge (keep) option, I will merge the old and the new directories. These new templates will be added: {}"#, combine_strings(new_template_names.filter(|t| !ts.contains(t))))
                    },
                    (ConflictBehavior::MergeKeep, TemplateCollisions::None) => {
                        format!(r#"Because you have chosen the merge (keep) option, I will merge the old and the new directories. There are no template collisions, so I will add these new templates: {}"#, combine_strings(new_template_names))
                    },
                    (ConflictBehavior::MergeKeep, TemplateCollisions::All(_)) => {
                        format!(r#"Because you have chosen the merge (keep) option, I will merge the old and the new directories. However, all the templates you are trying to add ({}) already exist in the inix directory ("{}"), so I will not do anything."#, combine_strings(new_template_names) , inix_dir.path.display())
                    },

                    // merge (replace)
                    (ConflictBehavior::MergeReplace, TemplateCollisions::Some(ts) ) => {
                        format!(r#"Because you have chosen the merge (replace) option, I will merge the old and the new directories. These templates will be overwritten: {}. When I'm done, all these templates will have been added or updated: {}"#, combine_strings(ts.into_iter()), combine_strings(new_template_names))
                    },
                    (ConflictBehavior::MergeReplace, TemplateCollisions::None) => {
                        format!(r#"Because you have chosen the merge (replace) option, I will merge the old and the new directories. There are no template collisions, so I will add these new templates: {}"#, combine_strings(new_template_names))
                    },

                    (ConflictBehavior::MergeReplace, TemplateCollisions::All(_)) => {
                        format!(r#"Because you have chosen the merge (replace) option, I will merge the old and the new directories. All the templates you are trying to add already exist in the inix directory ("{}"). I will add the following templates: {}"#, inix_dir.path.display(), combine_strings(new_template_names) )
                    },

                    // cancel
                    (ConflictBehavior::Cancel, _) => format!(r#"Because you have chosen the cancel option and the inix directory ("{}") already exists, I will not do anything"#, inix_dir.path.display())
                };

                println!("{msg}");
            }
        }
    } else {
        // copy templates over (into an inix directory)
        match (inix_dir.state, on_conflict) {
            (InixDirState::DoesNotExist, _) => {
                let _ = create_dir_all(inix_dir.path).with_context(|| {
                    format!(
                        r#"I was unable to create the inix directory "{}"."#,
                        inix_dir.path.display()
                    )
                })?;
                for template in templates {
                    let target = inix_dir.path.join(template.name());
                    fs::copy(template.path(), &target).with_context(|| {
                        format!(
                            r#"I was unable to copy the "{}" template from "{}" to "{}"."#,
                            template.name(),
                            template.path().display(),
                            target.display()
                        )
                    })?;
                }
            }

            (InixDirState::AlreadyExists { .. }, ConflictBehavior::Overwrite) => {
                remove_dir_all(inix_dir.path)?;
                let _ = create_dir_all(inix_dir.path).with_context(|| {
                    format!(
                        r#"I was unable to create the inix directory "{}"."#,
                        inix_dir.path.display()
                    )
                })?;
                for template in templates {
                    let target = inix_dir.path.join(template.name());
                    for (file_name, contents) in template.files() {
                        let file = target.join(file_name);
                        fs::write(&file, &contents).with_context(|| {
                            format!(
                                r#"I was unable to copy the "{}" template from "{}" to "{}"."#,
                                template.name(),
                                template.path().display(),
                                target.display()
                            )
                        })?
                    }
                }
            }
            (
                InixDirState::AlreadyExists {
                    template_collisions,
                },
                ConflictBehavior::MergeKeep,
            ) => {
                let templates_to_copy = match template_collisions {
                    TemplateCollisions::Some(ts) => templates
                        .iter()
                        .filter(|t| !ts.contains(&t.name()))
                        .map(|t| t.clone())
                        .collect(),
                    TemplateCollisions::None => templates,
                    TemplateCollisions::All(_) => vec![],
                };

                for template in templates_to_copy {
                    let target = inix_dir.path.join(template.name());
                    for (file_name, contents) in template.files() {
                        let file = target.join(file_name);
                        fs::write(&file, &contents).with_context(|| {
                            format!(
                                r#"I was unable to copy the "{}" template from "{}" to "{}"."#,
                                template.name(),
                                template.path().display(),
                                target.display()
                            )
                        })?
                    }
                }
            }
            (InixDirState::AlreadyExists { .. }, ConflictBehavior::MergeReplace) => {
                for template in templates {
                    let target = inix_dir.path.join(template.name());
                    fs::copy(template.path(), &target).with_context(|| {
                        format!(
                            r#"I was unable to copy the {} template from "{}" to "{}"."#,
                            template.name(),
                            template.path().display(),
                            target.display()
                        )
                    })?;
                }
            }
            (InixDirState::AlreadyExists { .. }, ConflictBehavior::Cancel) => {
                // intentionally left blank
            }
        }
    }

    // render base templates

    let handlebars = Handlebars::new();

    let nix_template = {
        match included_templates().get("base").unwrap().files {
            TemplateFiles::Both { nix, .. } => nix,
            TemplateFiles::Nix(_) => unreachable!(),
        }
    };

    // reg.render_file()
    // for now, let's just print it to standard out?
    let output = handlebars
        .render(
            nix_template,
            &format!(
                r#"{{ "templates": [ {} ] }}"#,
                // templates
                ["rust"]
                    .into_iter()
                    // .map(|t| format!(r#""{}""#, t.name()))
                    .join(",")
            ),
        )
        .unwrap();

    println!("{output}");

    Ok(())
}

fn combine_strings<T, Item>(strings: T) -> String
where
    Item: Display + Ord + Clone,
    T: Iterator<Item = Item> + Clone,
{
    let quote = |item: Item| format!(r#""{}""#, item.to_string());

    match strings.clone().count() {
        0 | 1 => strings.map(quote).collect(),
        2 => strings.map(quote).join(" and "),
        len => strings
            .enumerate()
            .map(|(index, value)| {
                if index == len - 1 {
                    format!(r#"and {}"#, quote(value))
                } else {
                    quote(value)
                }
            })
            .join(", "),
    }
}

fn prompt_for_conflict_behavior(inix_dir: &InixDir) -> anyhow::Result<ConflictBehavior> {
    let mut rl = Editor::<()>::new()?;

    #[derive(Debug, Clone, Copy)]
    struct PromptOption {
        description: &'static str,
        short_description: &'static str,
        conflict_behavior: ConflictBehavior,
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
            combine_strings(self.options.keys().sorted())
        }
    }

    impl Display for Prompt {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            writedoc!(
                f,
                r#"{}

                How would you like to proceed?
                {}

                Please enter exactly one option (one of {} [case-insensitive])."#,
                self.text,
                self.list_options(),
                self.list_option_keys()
            )
        }
    }

    let conflicting_templates = match &inix_dir.state {
        InixDirState::DoesNotExist => return Ok(ConflictBehavior::Cancel),
        InixDirState::AlreadyExists {
            template_collisions,
        } => template_collisions,
    };

    let prompt = match conflicting_templates {
        TemplateCollisions::None => Prompt {
            options: hash_map! {
                'A' => PromptOption {description:"Merge the two inix directories, adding your new templates to the existing directory?",short_description:"merge", conflict_behavior: ConflictBehavior::MergeKeep },
                'B' => PromptOption{description:"Overwrite the whole directory, removing everything that's in it and replacing it with the new templates?",short_description:"overwrite", conflict_behavior: ConflictBehavior::Overwrite },
                'C' => PromptOption {description:"Cancel the operation",short_description:"cancel", conflict_behavior: ConflictBehavior::Cancel }
            },
            text: inix_dir.conflict_description(),
        },
        TemplateCollisions::All(_) => Prompt {
            text: inix_dir.conflict_description(),
            options: hash_map! {
                'A' =>
                    PromptOption {description:"Overwrite the entire inix directory, removing anything that exists there already.",short_description:"overwrite", conflict_behavior: ConflictBehavior::Overwrite },
                'B' => PromptOption{description:"Add your templates to the inix directory, overwriting any templates that are there already, but leaving other templates untouched.",short_description:"merge-replace", conflict_behavior: ConflictBehavior::MergeReplace },
                'C' => PromptOption {description:"Cancel the operation",short_description:"cancel", conflict_behavior: ConflictBehavior::Cancel }
            },
        },
        TemplateCollisions::Some(_) => Prompt {
            text: inix_dir.conflict_description(),
            options: hash_map! {
                'A' => PromptOption {description:"Overwrite the entire inix directory, removing anything that exists there already.",short_description:"overwrite", conflict_behavior: ConflictBehavior::Overwrite },
                'B' => PromptOption{description:"Add your templates to the inix directory, overwriting any templates that are there already, but leaving other templates untouched.",short_description:"merge-replace", conflict_behavior: ConflictBehavior::MergeReplace },
                'C' => PromptOption{description:"Add your templates to the inix directory, but leaving any templates that exist already.",short_description:"merge-keep", conflict_behavior: ConflictBehavior::MergeKeep },
                'D' => PromptOption {description:"Cancel the operation",short_description:"cancel", conflict_behavior: ConflictBehavior::Cancel }
            },
        },
    };

    println!();
    println!("{}", prompt);
    loop {
        println!();
        println!(r#"Tip: You can enter "?" to display the options again."#);
        let readline = rl.readline(">> ");
        match readline {
            Ok(line) if line.trim() == "?" => {
                println!("{}", prompt);
            }
            Ok(line) => {
                match prompt
                    .options
                    .iter()
                    .find(|(c, _)| line.trim().eq_ignore_ascii_case(&*c.to_string()))
                {
                    Some((_, option)) => return Ok(option.conflict_behavior),
                    None => println!("\nSorry, I don't understand what you mean. Please use only the character corresponding to the option you want."),
                }
            }
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => {
                println!("\nUnderstood. I'll cancel the operation.");
                bail!("The operation was cancelled.");
            }
            Err(err) => {
                println!("\nErr, I got an error that I don't understand: {:?}", err);
                println!("\nPlease try again or quit the program (Ctrl+C)");
            }
        }
    }
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
