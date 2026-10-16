//! Skills: instructions for a kind of task that the assistant reads when it needs them, in the
//! Agent Skills format that other agents share: a folder holding `SKILL.md`, which is front
//! matter giving a `name` and a `description` between `---` lines, then Markdown. The system
//! prompt lists only names and descriptions; the model reads a skill whole with `load_skill`. The
//! MCP server offers the same skills as prompts.
//!
//! Where skills come from ([`Source`]):
//!
//! - built in;
//! - plugins: a plugin package can ship skills (`skills/<name>/SKILL.md`), named
//!   `<plugin>:<name>` and offered while the plugin is enabled;
//! - on the desktop and the command line, the user's: folders in the settings folder's `skills`
//!   folder, added with [`add`] or by hand;
//! - shared: `~/.agents/skills`, where other agents keep theirs. These are off until switched on,
//!   since they were written for other tools.
//!
//! A user's skill replaces a shared or built-in one of the same name. Which skills are switched
//! on is kept in the settings folder's `skills.json`.

use serde_json::json;

use crate::provider::ToolDef;

/// The largest `SKILL.md` read.
pub const MAX_SKILL: usize = 64 * 1024;

/// Where a skill comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    BuiltIn,
    /// A plugin's (its id).
    Plugin(String),
    /// The user's, in this folder.
    User(std::path::PathBuf),
    /// Shared with other agents (`~/.agents/skills`), in this folder.
    Shared(std::path::PathBuf),
}

/// A skill: what it's for, and its instructions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    /// Lower case, digits and hyphens; a plugin's skill is `<plugin>:<name>`.
    pub name: String,
    /// What it does and when to use it (all the model sees until it loads the skill).
    pub description: String,
    /// The instructions, in Markdown.
    pub body: String,
    pub source: Source,
}

/// The skills that come with RustGIS.
pub fn built_in() -> Vec<Skill> {
    [
        include_str!("../skills/thematic-map/SKILL.md"),
        include_str!("../skills/explore-data/SKILL.md"),
        include_str!("../skills/print-layout/SKILL.md"),
        include_str!("../skills/geoprocessing/SKILL.md"),
        include_str!("../skills/add-data/SKILL.md"),
        include_str!("../skills/share-web-map/SKILL.md"),
    ]
    .iter()
    .filter_map(|text| parse(text).ok())
    .collect()
}

/// Reads a `SKILL.md`: its front matter's `name` and `description`, then the body. Its source is
/// [`Source::BuiltIn`] until the caller says otherwise.
pub fn parse(text: &str) -> Result<Skill, String> {
    let text = text.trim_start_matches('\u{feff}');
    let rest = text.strip_prefix("---").ok_or("a skill starts with front matter between --- lines")?;
    let (front, body) = rest.split_once("\n---").ok_or("the front matter has no closing --- line")?;
    let lines: Vec<&str> = front.lines().collect();
    let field = |key: &str| {
        let i = lines.iter().position(|l| l.split_once(':').is_some_and(|(k, _)| k.trim() == key))?;
        let value = lines.get(i)?.split_once(':')?.1.trim();
        // A folded (`>`) or literal (`|`) block: the indented lines after it, joined.
        if value.starts_with(['>', '|']) {
            let block: Vec<&str> =
                lines.iter().skip(i + 1).take_while(|l| l.starts_with([' ', '\t']) || l.trim().is_empty()).map(|l| l.trim()).collect();
            return Some(block.join(" ").split_whitespace().collect::<Vec<_>>().join(" "));
        }
        Some(value.trim_matches(|c| c == '"' || c == '\'').to_string())
    };
    let name = field("name").filter(|n| !n.is_empty()).ok_or("the front matter has no name")?;
    if !valid_name(&name) {
        return Err(format!("'{name}': a skill's name is lower case letters, digits and hyphens"));
    }
    let description = field("description").filter(|d| !d.is_empty()).ok_or("the front matter has no description")?;
    let body = body.trim_start_matches(|c| c != '\n').trim().to_string();
    Ok(Skill { name, description: description.chars().take(1024).collect(), body, source: Source::BuiltIn })
}

/// A skill's own name: lower case letters, digits and hyphens, at most 64.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `extra` added to `skills`, a skill of the same name replacing one.
pub fn with(mut skills: Vec<Skill>, extra: Vec<Skill>) -> Vec<Skill> {
    for s in extra {
        skills.retain(|k| k.name != s.name);
        skills.push(s);
    }
    skills
}

/// The skills enabled plugins ship, as `<plugin>:<name>`, and the problems with any that can't be
/// read.
pub fn from_plugins() -> (Vec<Skill>, Vec<String>) {
    let (mut skills, mut problems) = (Vec::new(), Vec::new());
    for p in rustgis_engine::plugins::skills() {
        match parse(&p.text) {
            Ok(s) if s.name == p.name => {
                skills.push(Skill { name: format!("{}:{}", p.plugin, p.name), source: Source::Plugin(p.plugin), ..s });
            }
            Ok(s) => problems.push(format!("{}:{}: its SKILL.md is named '{}'", p.plugin, p.name, s.name)),
            Err(e) => problems.push(format!("{}:{}: {e}", p.plugin, p.name)),
        }
    }
    (skills, problems)
}

/// What the system prompt says about the skills there are.
pub fn catalog(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out =
        String::from("\n\nSkills (instructions for kinds of task): when a request matches one, read it with load_skill before acting.");
    for s in skills {
        out.push_str(&format!("\n- {}: {}", s.name, s.description));
    }
    out
}

/// The `load_skill` tool.
pub fn tool() -> ToolDef {
    ToolDef {
        name: "load_skill",
        description: "Read a skill's instructions by name (the system prompt lists the skills). Read it before doing a task it covers.",
        schema: json!({
            "type": "object",
            "properties": {"name": {"type": "string", "description": "The skill's name"}},
            "required": ["name"]
        }),
    }
}

/// Runs `load_skill`.
pub fn load(skills: &[Skill], name: &str) -> Result<String, String> {
    let name = name.trim();
    skills.iter().find(|s| s.name == name).map(|s| s.body.clone()).ok_or_else(|| {
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        format!("no skill '{name}'; there are: {}", names.join(", "))
    })
}

/// The skills the assistant uses here: on the desktop and the command line, those switched on
/// ([`list`]); in the browser, the built-in ones and the plugins'.
pub fn installed() -> Vec<Skill> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        list(&Places::here()).0.into_iter().filter(|l| l.on).map(|l| l.skill).collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        with(built_in(), from_plugins().0)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use local::*;

/// The user's and shared skills, and which are switched on (desktop and command line).
#[cfg(not(target_arch = "wasm32"))]
mod local {
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use super::{MAX_SKILL, Skill, Source, built_in, from_plugins, parse, with};

    /// The most skills read from one folder or archive.
    const MAX_SKILLS: usize = 200;
    /// The largest skills archive [`add`] reads.
    const MAX_ARCHIVE: u64 = 16 * 1024 * 1024;

    /// The folders skills are kept in.
    #[derive(Clone, Debug, Default)]
    pub struct Places {
        /// The user's skills: one folder each.
        pub user: Option<PathBuf>,
        /// Skills shared with other agents.
        pub shared: Option<PathBuf>,
        /// Which are switched on or off (`skills.json`).
        pub state: Option<PathBuf>,
    }

    impl Places {
        /// The usual folders: `skills` and `skills.json` in the settings folder, and
        /// `~/.agents/skills`.
        pub fn here() -> Places {
            let settings = crate::chatgpt::settings_dir();
            let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).filter(|h| !h.is_empty());
            Places {
                user: settings.as_ref().map(|d| d.join("skills")),
                shared: home.map(|h| PathBuf::from(h).join(".agents").join("skills")),
                state: settings.map(|d| d.join("skills.json")),
            }
        }
    }

    /// A skill and whether the assistant uses it.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Listed {
        pub skill: Skill,
        pub on: bool,
    }

    /// Every skill (built-in, shared, the user's, the plugins'), whether each is on, and the
    /// problems with any that couldn't be read.
    pub fn list(places: &Places) -> (Vec<Listed>, Vec<String>) {
        let mut problems = Vec::new();
        let mut from = |dir: &Option<PathBuf>, shared: bool| {
            let (skills, p) = dir.as_deref().map(load_dir).unwrap_or_default();
            problems.extend(p);
            skills
                .into_iter()
                .map(|s| match (shared, s.source) {
                    (true, Source::User(d)) => Skill { source: Source::Shared(d), ..s },
                    (_, source) => Skill { source, ..s },
                })
                .collect::<Vec<_>>()
        };
        let shared = from(&places.shared, true);
        let user = from(&places.user, false);
        let (plugins, p) = from_plugins();
        problems.extend(p);
        let state = read_state(places);
        let skills = with(with(with(built_in(), shared), user), plugins);
        let listed = skills.into_iter().map(|skill| Listed { on: is_on(&state, &skill), skill }).collect();
        (listed, problems)
    }

    /// Whether `skill` is on: a shared one when switched on, any other unless switched off.
    fn is_on(state: &Value, skill: &Skill) -> bool {
        let names = |key: &str| state[key].as_array().is_some_and(|a| a.iter().any(|n| n.as_str() == Some(skill.name.as_str())));
        match skill.source {
            Source::Shared(_) => names("enabled"),
            _ => !names("disabled"),
        }
    }

    fn read_state(places: &Places) -> Value {
        places
            .state
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}))
    }

    /// Switches a skill on or off, and remembers it.
    pub fn set_enabled(places: &Places, name: &str, on: bool) -> Result<(), String> {
        let skill = list(places).0.into_iter().find(|l| l.skill.name == name).ok_or_else(|| format!("no skill '{name}'"))?.skill;
        let mut state = read_state(places);
        let mut names = |key: &str, keep: bool| {
            let mut v: Vec<Value> = state[key].as_array().cloned().unwrap_or_default();
            v.retain(|n| n.as_str() != Some(name));
            if keep {
                v.push(json!(name));
            }
            state[key] = Value::Array(v);
        };
        let shared = matches!(skill.source, Source::Shared(_));
        names("enabled", shared && on);
        names("disabled", !shared && !on);
        let path = places.state.as_ref().ok_or("there is no settings folder")?;
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let text = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
        rustgis_engine::data::io::write_atomic(path, text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Adds skills to the user's: a `SKILL.md`, a skill's folder, a folder of skill folders, or
    /// a `.zip` of any of these. Each is kept as `<name>/SKILL.md`, replacing a user's skill of
    /// the same name. Only `SKILL.md` is kept: the assistant reads nothing else. Returns the
    /// names added; nothing is added if any can't be read.
    pub fn add(places: &Places, path: &Path) -> Result<Vec<String>, String> {
        let texts = if path.is_dir() {
            if path.join("SKILL.md").is_file() {
                vec![read_skill(&path.join("SKILL.md"))?]
            } else {
                let (skills, problems) = load_dir(path);
                if let Some(p) = problems.into_iter().next() {
                    return Err(p);
                }
                skills.iter().map(skill_file).collect::<Result<Vec<_>, _>>()?
            }
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
            from_zip(path)?
        } else {
            vec![read_skill(path)?]
        };
        if texts.is_empty() {
            return Err(format!("{}: no SKILL.md found", path.display()));
        }
        let skills = texts.iter().map(|t| parse(t).map(|s| (s.name, t))).collect::<Result<Vec<_>, _>>()?;
        let dir = places.user.as_ref().ok_or("there is no settings folder")?;
        // Where an existing skill of each name is (its folder may be named otherwise).
        let mine = load_dir(dir).0;
        let mut names = Vec::new();
        for (name, text) in skills {
            let folder = mine
                .iter()
                .find_map(|s| match &s.source {
                    Source::User(d) if s.name == name => Some(d.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| dir.join(&name));
            std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
            let file = folder.join("SKILL.md");
            rustgis_engine::data::io::write_atomic(&file, text.as_bytes()).map_err(|e| format!("{}: {e}", file.display()))?;
            names.push(name);
        }
        Ok(names)
    }

    /// Removes one of the user's skills (its folder). Built-in, shared and plugin skills can only
    /// be switched off.
    pub fn remove(places: &Places, name: &str) -> Result<(), String> {
        let dir = places.user.as_ref().ok_or("there is no settings folder")?;
        let folder = load_dir(dir)
            .0
            .into_iter()
            .find_map(|s| match s.source {
                Source::User(d) if s.name == name => Some(d),
                _ => None,
            })
            .ok_or_else(|| format!("'{name}' isn't one of your skills (others can be switched off)"))?;
        std::fs::remove_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))
    }

    /// A skill's `SKILL.md`, read again from its folder.
    fn skill_file(s: &Skill) -> Result<String, String> {
        match &s.source {
            Source::User(d) | Source::Shared(d) => read_skill(&d.join("SKILL.md")),
            _ => Err(format!("{}: not in a folder", s.name)),
        }
    }

    fn read_skill(path: &Path) -> Result<String, String> {
        let len = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
        if len as usize > MAX_SKILL {
            return Err(format!("{}: larger than 64 KB", path.display()));
        }
        std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Every `SKILL.md` in a zip archive.
    fn from_zip(path: &Path) -> Result<Vec<String>, String> {
        use std::io::Read;
        let len = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
        if len > MAX_ARCHIVE {
            return Err(format!("{}: larger than 16 MB", path.display()));
        }
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("{}: not a zip archive: {e}", path.display()))?;
        let names: Vec<String> =
            zip.file_names().filter(|n| *n == "SKILL.md" || n.ends_with("/SKILL.md")).map(str::to_string).take(MAX_SKILLS).collect();
        let mut texts = Vec::new();
        for name in names {
            let f = zip.by_name(&name).map_err(|e| format!("{name}: {e}"))?;
            let mut text = String::new();
            f.take(MAX_SKILL as u64 + 1).read_to_string(&mut text).map_err(|e| format!("{name}: {e}"))?;
            if text.len() > MAX_SKILL {
                return Err(format!("{name}: larger than 64 KB"));
            }
            texts.push(text);
        }
        Ok(texts)
    }

    /// The skills in `dir`: each folder holding a `SKILL.md`, as [`Source::User`]. Unreadable or
    /// malformed ones are left out, with why.
    pub fn load_dir(dir: &Path) -> (Vec<Skill>, Vec<String>) {
        let (mut skills, mut problems) = (Vec::new(), Vec::new());
        let Ok(entries) = std::fs::read_dir(dir) else { return (skills, problems) };
        let mut folders: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.join("SKILL.md").is_file()).collect();
        folders.sort();
        for folder in folders.into_iter().take(MAX_SKILLS) {
            let path = folder.join("SKILL.md");
            match read_skill(&path).and_then(|text| parse(&text).map_err(|e| format!("{}: {e}", path.display()))) {
                Ok(s) => skills.push(Skill { source: Source::User(folder), ..s }),
                Err(e) => problems.push(e),
            }
        }
        (skills, problems)
    }

    /// Where the user's skills are: the `skills` folder in the app's settings folder.
    pub fn user_dir() -> Option<PathBuf> {
        Places::here().user
    }
}
