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

impl Default for ConflictBehavior {
    fn default() -> Self {
        ConflictBehavior::Cancel
    }
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
    /// If the directory does not already exist, then inix will try to create it.
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

impl Default for Cli {
    fn default() -> Self {
        Self {
            templates: Default::default(),
            directory: Default::default(),
            dry_run: Default::default(),
            auto_allow: Default::default(),
            on_conflict: Default::default(),
        }
    }
}

fn try_get_target_dir(input: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    match input {
        None => current_dir().context("Failed to read the current working directory."),

        Some(dir) => {
            if dir.is_dir() || !dir.exists() {
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
enum TemplateFiles2 {
    Nix(String),
    Envrc(String),
    Both { nix: String, envrc: String },
}

#[derive(Clone, Debug, Copy)]
enum TemplateType {
    Custom,
    Builtin,
}

#[derive(Clone, Debug)]
struct Template2 {
    name: String,
    files: TemplateFiles2,
    source_dir: PathBuf,
    template_type: TemplateType,
}

impl Template2 {
    fn name(&self) -> &str {
        &self.name
    }

    fn path(&self) -> PathBuf {
        self.source_dir.join(self.name.to_string())
    }

    fn files(&self) -> Vec<(&'static str, &str)> {
        match &self.files {
            TemplateFiles2::Nix(content) => vec![("shell.nix", &content)],
            TemplateFiles2::Envrc(content) => vec![(".envrc", &content)],
            TemplateFiles2::Both { nix, envrc } => {
                vec![(".envrc", &envrc), ("shell.nix", &nix)]
            }
        }
    }
}

fn included_templates() -> HashMap<&'static str, Template2> {
    hash_map! {
        "rust" => Template2 {name:"rust".into(),files:TemplateFiles2::Nix(include_str!("templates/rust/shell.nix").into()),source_dir:PathBuf::from("inix/templates"), template_type: TemplateType::Builtin},
        "node" => Template2 {
            name: "node".into(),
            files: TemplateFiles2::Both {
                nix: include_str!("templates/node/shell.nix").into(),
                envrc: include_str!("templates/node/.envrc").into(),
            },
            source_dir: PathBuf::from("inix/templates")
                , template_type: TemplateType::Builtin
        },
        "base" =>  Template2 {
            name: "base".into(),
            files: TemplateFiles2::Both {
                nix: include_str!("templates/base/shell.nix.template").into(),
              envrc: include_str!("templates/base/.envrc.template").into(),
            },
            source_dir: PathBuf::from("inix/templates"), template_type: TemplateType::Builtin
        },
    }
}

fn try_get_templates(input_templates: &[String]) -> anyhow::Result<Vec<Template2>> {
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
                        (Ok(nix), Err(_)) => Some(TemplateFiles2::Nix(nix)),
                        (Err(_), Ok(envrc)) => Some(TemplateFiles2::Envrc(envrc)),
                        (Ok(nix), Ok(envrc)) => Some(TemplateFiles2::Both { nix, envrc }),
                    }
                    .map(|files| Template2 {
                        name: template_name.to_owned(),
                        source_dir: dir,
                        files,
                        template_type: TemplateType::Custom,
                    })
                })
                .or_else(|| {
                    included_templates()
                        .get(&template_name as &str)
                        .map(|t| t.clone())
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

fn run(cli: Cli) -> anyhow::Result<()> {
    // PREPARE //

    // check to see whether we can find all the templates
    let templates = try_get_templates(&cli.templates)?;

    // check to see if the target directory exists
    let target_dir = try_get_target_dir(cli.directory)?;

    // check to see whether we have write permissions in the target
    // directory

    // does the inix subdirectory already exist?
    let inix_dir_path = target_dir.join("inix");

    // gather all data about the inix dir

    let inix_dir = {
        let state = if inix_dir_path.is_dir() {
            let conflicting_templates: Vec<&str> = templates
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

                let new_template_names = templates.iter().map(Template2::name);

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
        if !target_dir.exists() {
            create_dir_all(&target_dir).with_context(|| {
                format!(
                    r#"I was unable to create the target project dir ("{}")"#,
                    &target_dir.display()
                )
            })?
        } else {
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
        }

        // copy templates over (into an inix directory)
        match (inix_dir.state, on_conflict) {
            (InixDirState::DoesNotExist, _) => {
                let _ = create_dir_all(inix_dir.path).with_context(|| {
                    format!(
                        r#"I was unable to create the inix directory "{}"."#,
                        inix_dir.path.display()
                    )
                })?;
                for template in &templates {
                    let target = inix_dir.path.join(template.name());
                    create_dir_all(&target).with_context(|| {
                        format!(
                            r#"I was unable to create the template directory "{}"."#,
                            target.display()
                        )
                    })?;
                    for (file_name, contents) in template.files() {
                        let file = target.join(file_name);
                        fs::write(&file, &contents).with_context(|| {
                            format!(
                                r#"I was unable to write the "{}" template (found at "{}") to "{}"."#,
                                template.name(),
                                template.path().display(),
                                target.display()
                            )
                        })?
                    }
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
                for template in &templates {
                    let target = inix_dir.path.join(template.name());
                    create_dir_all(&target).with_context(|| {
                        format!(
                            r#"I was unable to create the template directory "{}"."#,
                            target.display()
                        )
                    })?;
                    for (file_name, contents) in template.files() {
                        let file = target.join(file_name);
                        fs::write(&file, &contents).with_context(|| {
                            format!(
                                r#"I was unable to write the "{}" template (found at "{}") to "{}"."#,
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
                    TemplateCollisions::None => templates.clone(),
                    TemplateCollisions::All(_) => vec![],
                };

                for template in templates_to_copy {
                    let target = inix_dir.path.join(template.name());
                    create_dir_all(&target).with_context(|| {
                        format!(
                            r#"I was unable to create the template directory "{}"."#,
                            target.display()
                        )
                    })?;
                    for (file_name, contents) in template.files() {
                        let file = target.join(file_name);
                        fs::write(&file, &contents).with_context(|| {
                            format!(
                                r#"I was unable to write the "{}" template (found at "{}") to "{}"."#,
                                template.name(),
                                template.path().display(),
                                target.display()
                            )
                        })?
                    }
                }
            }
            (InixDirState::AlreadyExists { .. }, ConflictBehavior::MergeReplace) => {
                for template in &templates {
                    let target = inix_dir.path.join(template.name());
                    create_dir_all(&target).with_context(|| {
                        format!(
                            r#"I was unable to create the template directory "{}"."#,
                            target.display()
                        )
                    })?;
                    for (file_name, contents) in template.files() {
                        let file = target.join(file_name);
                        fs::write(&file, &contents).with_context(|| {
                            format!(
                                r#"I was unable to write the "{}" template (found at "{}") to "{}"."#,
                                template.name(),
                                template.path().display(),
                                target.display()
                            )
                        })?
                    }
                }
            }
            (InixDirState::AlreadyExists { .. }, ConflictBehavior::Cancel) => {
                // intentionally left blank
            }
        }
    }

    // render base templates
    let handlebars = Handlebars::new();

    let (nix_template, envrc_template) = {
        match &included_templates().get("base").unwrap().files {
            TemplateFiles2::Both { nix, envrc } => (nix.clone(), envrc.clone()),
            TemplateFiles2::Nix(_) | TemplateFiles2::Envrc(_) => unreachable!(),
        }
    };

    let handlebars_args = hash_map! {
       "templates" =>  templates.iter().map(Template2::name).collect::<Vec<_>>()
    };

    // reg.render_file()
    // for now, let's just print it to standard out?

    handlebars.render_template_to_write(
        &nix_template,
        &handlebars_args,
        &fs::File::create(target_dir.join("shell.nix"))?,
    )?;

    handlebars.render_template_to_write(
        &envrc_template,
        &handlebars_args,
        &fs::File::create(target_dir.join(".envrc"))?,
    )?;
    // .render_template(&nix_template, &handlebars_args)

    // println!("{}", fs::read_to_string(inix_dir.path.join("shell.nix"))?);

    // println!("{output}, {handlebars_args:?}");

    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    run(cli)
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

    use std::{collections::HashSet, ops::Deref, path::Path, time::SystemTime};

    use proptest::prelude::*;
    use tempfile::tempdir;

    use super::*;

    #[test]
    /// Verify that the CLI is configured correctly.
    fn verify_cli() {
        use clap::CommandFactory;
        Cli::command().debug_assert()
    }

    struct InixPaths<'a> {
        base_dir: &'a Path,
        inix_dir: &'a Path,
        shell_nix: &'a Path,
        envrc: &'a Path,
    }

    fn test_inix_with_setup<T, SetupOutput>(
        args: Cli,
        setup: impl FnOnce(&InixPaths) -> SetupOutput,
        execute: impl FnOnce(&InixPaths, SetupOutput) -> T,
    ) {
        let target_dir = args
            .directory
            .unwrap_or(tempdir().expect("couldn't create a temp dir").path().into());

        let paths = InixPaths {
            base_dir: &target_dir,
            inix_dir: &target_dir.join("inix"),
            shell_nix: &target_dir.join("shell.nix"),
            envrc: &target_dir.join("shell.nix"),
        };

        let args_p = Cli {
            directory: Some(target_dir.clone()),
            ..args
        };

        let setup_output = setup(&paths);

        match run(args_p) {
            Err(e) => assert!(
                false,
                r#"Running the inix program failed with an error: {e:?}"#
            ),
            Ok(_) => {
                execute(&paths, setup_output);
            }
        };
    }

    fn test_inix<T>(args: Cli, execute: impl FnOnce(&InixPaths) -> T) {
        test_inix_with_setup(args, |_| {}, |paths, _| execute(paths))
    }

    fn power_set<'a, T>(a: &[T]) -> impl Iterator<Item = &[T]> {
        std::iter::once([].as_ref()).chain(
            (0..=a.len())
                .tuple_combinations()
                .map(move |(start, end)| &a[start..end]),
        )
    }

    // Test cases
    //
    // - it completes successfully without templates
    #[test]
    fn it_works_without_provided_templates() {
        test_inix(
            Cli {
                templates: vec![],
                ..Default::default()
            },
            |paths| {
                for expected_file in [paths.shell_nix, paths.envrc] {
                    assert!(
                        expected_file.is_file(),
                        r#"The file "/{}" does not exist or is not a file."#,
                        expected_file.display()
                    );
                }
                assert_eq!(
                    false,
                    paths.inix_dir.exists(),
                    r#"The /inix directory was created when it shouldn't have anything in it."#
                );
            },
        )
    }

    // - creates shell.nix, .envrc, and inix/* files
    // - creates any directories necessary if they don't exist
    #[test]
    fn it_creates_files() {
        let base_dir = tempdir().unwrap();

        let project_dir = base_dir.path().join("my/project");

        for templates in power_set(&["rust", "node"]).filter(|set| set.len() > 0) {
            let args = Cli {
                templates: templates.iter().map(|s| s.deref().into()).collect(),
                directory: Some(project_dir.clone()),
                ..Default::default()
            };

            test_inix(args, |paths| {
                for expected_file in [paths.shell_nix, paths.envrc] {
                    assert!(
                        expected_file.is_file(),
                        r#"The file "/{}" does not exist or is not a file."#,
                        expected_file.display()
                    );
                }

                for expected_file in ["node/shell.nix", "node/.envrc"] {
                    assert!(
                        paths.inix_dir.join(expected_file).is_file(),
                        r#"The file "/{expected_file}" does not exist or is not a file."#
                    );
                }
            })
        }
    }

    //
    // - the resulting .envrc and shell.nix files actually work
    #[test]
    fn the_envrc_file_works() {
        // todo: use proptest to generate this with and without
        // subdirectories that it needs to source from?
        todo!()
    }

    #[test]
    fn the_nix_file_works() {
        // todo: use proptest to generate this with and without
        // subdirectories that it needs to source from?
        todo!()
    }

    // - the base .envrc and shell.nix files contain links to all the
    // templates mentioned
    #[test]
    fn all_templates_are_linked() {
        // in short:
        // generate a set of templates. Ensure that each one is linked in both .envrc and shell.nix
        todo!()
    }

    // - merge-replace: overwrites conflicting files
    //
    #[test]
    fn merge_replace() {
        proptest!(|(
            nix: bool,
            envrc: bool,
            existing_templates in prop::collection::hash_set("node|rust", 0..2),
            new_templates in prop::collection::hash_set("node|rust", 0..2))|
                  go(nix, envrc, existing_templates, new_templates)
        );

        fn go(
            nix: bool,
            envrc: bool,
            existing_templates: HashSet<String>,
            new_templates: HashSet<String>,
        ) {
            let templates = vec!["node".into()];
            let num_templates = templates.len();
            let args = Cli {
                templates,
                ..Default::default()
            };

            test_inix_with_setup(
                args,
                |paths| {
                    if nix {
                        fs::File::create(paths.shell_nix).unwrap();
                    }
                    if envrc {
                        fs::File::create(paths.envrc).unwrap();
                    }

                    for dir in existing_templates.iter() {
                        let subdir = paths.inix_dir.join(dir);
                        create_dir_all(&subdir).unwrap();
                        fs::File::create(subdir.join("shell.nix_placeholder")).unwrap();
                    }

                    SystemTime::now()
                },
                |paths, timestamp| {
                    prop_assert!(SystemTime::now() > timestamp);

                    // for dir in existing_templates.iter() {
                    //     let subdir = inix_dir.join(dir);
                    //     create_dir_all(&subdir).unwrap();
                    //     fs::File::create(subdir.join("shell.nix_placeholder")).unwrap();
                    // }

                    for expected_file in ["node/shell.nix", "node/.envrc"] {
                        prop_assert!(
                            paths.inix_dir.join(expected_file).exists(),
                            r#"The file "/{expected_file}" does not exist."#
                        );
                    }

                    // the inix directory only contains as many subdirs as there are templates
                    let num_created_templates = fs::read_dir(paths.inix_dir)
                    .expect(&format!(
                        r#"I was unable to read the inix directory that I expected to find at "{}""#,
                        paths.inix_dir.display()
                    ))
                    .count();

                    prop_assert_eq!(
                        num_templates,
                        num_created_templates,
                        "I expected to find {} templates in the inix dir, but I actually found {}.",
                        num_templates,
                        num_created_templates
                    );

                    for file in [paths.shell_nix, paths.envrc] {
                        let content = fs::read_to_string(file).map(|s| s.len()).context(format!(
                            r#"I was unable to read the file "/{}""#,
                            &file.display()
                        ));

                        prop_assert!(
                            content.unwrap_or(0) > 0,
                            r#"The file "/{}" has no content."#,
                            &file.display()
                        )
                    }

                    Ok(())
                },
            )
        }
    }

    // - merge-keep: does not overwrite conflicting files
    //
    //   - if there are existing shell.nix and/or .envrc files: can
    //   these be renamed with a timestamp and sourced? Or we could
    //   give them "generations". If a conflicting is discovered, take
    //   the highest generation found and make a new one. What if
    //   there are gaps? E.g. gens 1,2,7? Then do gen 8.
    //
    // - cancel: cancels on existing files
    //
    // - auto-allow performs the necessary functions
    //
    // - nothing is written if --dry-run is provided

    // - overwrites existing files and dirs if asked to
    // proptest! {

    #[test]
    fn it_overwrites_files() {
        proptest!(|(
            nix: bool,
            envrc: bool,
            subdirs in prop::collection::vec("[a-zA-Z0-9]+", 0..10))|
                  go(nix, envrc, subdirs)
        );

        fn go(nix: bool, envrc: bool, subdirs: Vec<String>) {
            let templates = vec!["node".into()];
            let num_templates = templates.len();
            let args = Cli {
                templates,
                ..Default::default()
            };

            test_inix(args, |paths| {
                if nix {
                    fs::File::create(paths.shell_nix).unwrap();
                }
                if envrc {
                    fs::File::create(paths.envrc).unwrap();
                }

                for dir in subdirs.iter() {
                    let subdir = paths.inix_dir.join(dir);
                    create_dir_all(&subdir).unwrap();
                    fs::File::create(subdir.join("shell.nix_placeholder")).unwrap();
                }

                for expected_file in ["node/shell.nix", "node/.envrc"] {
                    prop_assert!(
                        paths.inix_dir.join(expected_file).exists(),
                        r#"The file "/{expected_file}" does not exist."#
                    );
                }

                // the inix directory only contains as many subdirs as there are templates
                let num_created_templates = fs::read_dir(paths.inix_dir)
                .expect(&format!(
                    r#"I was unable to read the inix directory that I expected to find at "{}""#,
                    paths.inix_dir.display()
                ))
                .count();

                prop_assert_eq!(
                    num_templates,
                    num_created_templates,
                    "I expected to find {} templates in the inix dir, but I actually found {}.",
                    num_templates,
                    num_created_templates
                );

                for file in [paths.shell_nix, paths.envrc] {
                    let content = fs::read_to_string(file).map(|s| s.len()).context(format!(
                        r#"I was unable to read the file "/{}""#,
                        &file.display()
                    ));

                    prop_assert!(
                        content.unwrap_or(0) > 0,
                        r#"The file "/{}" has no content."#,
                        &file.display()
                    )
                }

                Ok(())
            })
        }
    }

    // - it does not touch an existing inix dir if it has no templates to write
    //
    // In cases where you don't provide it with any templates, inix
    // will not try to write an inix dir. However, if you ask inix to
    // overwrite on conflict, it will detect that this directory
    // already exists. In these cases, it should err on the side of
    // caution and not remove the existing directory.
    #[test]
    fn it_doesnt_overwrite_inix_dir_if_it_has_nothing_to_write() {
        let template_dir = "inix/template";
        test_inix_with_setup(
            Cli {
                templates: vec![],
                on_conflict: Some(ConflictBehavior::Overwrite),
                ..Default::default()
            },
            |paths| {
                create_dir_all(paths.base_dir.join(&template_dir))
                    .expect("Failed to create a pre-existing template dir to set up the test.");
            },
            |paths, _| {
                assert!(
                    paths.base_dir.join(&template_dir).exists(),
                    "The pre-existing template directory does not exist anymore"
                );
            },
        )
    }
}
