//! Skills: instructions for a kind of task that the assistant reads when it needs them, in the
//! Agent Skills format that other agents share: a folder holding `SKILL.md`, which is front
//! matter giving a `name` and a `description` between `---` lines, then Markdown. The system
//! prompt lists only names and descriptions; the model reads a skill whole with `load_skill`. The
//! MCP server offers the same skills as prompts.
//!
//! A few skills are built in; on the desktop and the command line, more can be added as folders
//! in the settings folder's `skills` folder ([`load_dir`]). A user's skill of the same name
//! replaces a built-in one.

use serde_json::json;

use crate::provider::ToolDef;

/// The largest `SKILL.md` read.
pub const MAX_SKILL: usize = 64 * 1024;

/// A skill: what it's for, and its instructions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    /// Lower case, digits and hyphens.
    pub name: String,
    /// What it does and when to use it (all the model sees until it loads the skill).
    pub description: String,
    /// The instructions, in Markdown.
    pub body: String,
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

/// Reads a `SKILL.md`: its front matter's `name` and `description`, then the body.
pub fn parse(text: &str) -> Result<Skill, String> {
    let text = text.trim_start_matches('\u{feff}');
    let rest = text.strip_prefix("---").ok_or("a skill starts with front matter between --- lines")?;
    let (front, body) = rest.split_once("\n---").ok_or("the front matter has no closing --- line")?;
    let field = |key: &str| {
        front.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == key).then(|| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
        })
    };
    let name = field("name").filter(|n| !n.is_empty()).ok_or("the front matter has no name")?;
    if name.len() > 64 || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(format!("'{name}': a skill's name is lower case letters, digits and hyphens"));
    }
    let description = field("description").filter(|d| !d.is_empty()).ok_or("the front matter has no description")?;
    let body = body.trim_start_matches(|c| c != '\n').trim().to_string();
    Ok(Skill { name, description: description.chars().take(1024).collect(), body })
}

/// `extra` added to the built-in skills, a skill of the same name replacing one.
pub fn with(mut skills: Vec<Skill>, extra: Vec<Skill>) -> Vec<Skill> {
    for s in extra {
        skills.retain(|k| k.name != s.name);
        skills.push(s);
    }
    skills
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

/// The skills in `dir`: each folder holding a `SKILL.md`. Unreadable or malformed ones are left
/// out, with why.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_dir(dir: &std::path::Path) -> (Vec<Skill>, Vec<String>) {
    let (mut skills, mut problems) = (Vec::new(), Vec::new());
    let Ok(entries) = std::fs::read_dir(dir) else { return (skills, problems) };
    let mut paths: Vec<std::path::PathBuf> =
        entries.filter_map(|e| e.ok().map(|e| e.path().join("SKILL.md"))).filter(|p| p.is_file()).collect();
    paths.sort();
    for path in paths.into_iter().take(200) {
        let read = std::fs::metadata(&path)
            .map_err(|e| e.to_string())
            .and_then(|m| if m.len() as usize > MAX_SKILL { Err("larger than 64 KB".into()) } else { Ok(()) })
            .and_then(|()| std::fs::read_to_string(&path).map_err(|e| e.to_string()));
        match read.and_then(|text| parse(&text)) {
            Ok(s) => skills.push(s),
            Err(e) => problems.push(format!("{}: {e}", path.display())),
        }
    }
    (skills, problems)
}

/// Where the user's skills are: the `skills` folder in the app's settings folder.
#[cfg(not(target_arch = "wasm32"))]
pub fn user_dir() -> Option<std::path::PathBuf> {
    crate::chatgpt::settings_dir().map(|d| d.join("skills"))
}

/// The built-in skills and the user's.
#[cfg(not(target_arch = "wasm32"))]
pub fn installed() -> Vec<Skill> {
    let user = user_dir().map(|d| load_dir(&d).0).unwrap_or_default();
    with(built_in(), user)
}
