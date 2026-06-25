//! Agent Skills support (progressive disclosure, like Claude Code).
//!
//! A "skill" is a directory containing a `SKILL.md` with YAML frontmatter (`name`, `description`)
//! and optional bundled scripts/references — the format produced by `npx skills add …` and the
//! Agent Skills spec. We scan configured roots for these, inject a compact catalog (name +
//! one-line description) into the cached system prompt so EVERY model (cloud or local) knows what
//! skills exist, and expose a `use_skill` tool that loads a skill's full `SKILL.md` on demand. The
//! skill body then drives the agent (which runs the skill's tools via `run_command`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// One discovered skill.
#[derive(Clone, Debug)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Directory containing `SKILL.md` (and any bundled files).
    pub dir: PathBuf,
}

/// The set of skills discovered on disk.
#[derive(Clone, Debug, Default)]
pub struct SkillCatalog {
    skills: Vec<Skill>,
    /// Pre-formatted full CTF playbook section (solve-challenge body, slash-commands rewritten,
    /// capped, with header) for injection into the cached cloud-mode prompt. Built once at discover
    /// time so it isn't re-rendered every turn. `None` when solve-challenge isn't installed.
    preload_full: Option<String>,
}

/// Per-skill description cap in the catalog (the always-on prompt line).
const DESC_CAP: usize = 200;
/// `SKILL.md` (router) cap when loaded via `use_skill`. Big skills are an index that routes to
/// technique files; the generated file list (always appended) covers routing even if the body is cut.
const SKILL_BODY_CAP: usize = 12_000;
/// Cap when loading a specific bundled technique file via `use_skill(name, file=…)`.
const SKILL_FILE_CAP: usize = 16_000;
/// Defensive cap on the `solve-challenge` body injected into the cached prompt. The stock skill is
/// ~9k chars; this only guards against a pathologically large replacement.
const PRELOAD_BODY_CAP: usize = 14_000;
/// How deep to recurse looking for `SKILL.md` (category dirs may nest one or two levels).
const MAX_DEPTH: usize = 3;

impl SkillCatalog {
    pub fn empty() -> Self {
        Self { skills: Vec::new(), preload_full: None }
    }

    /// Scan `roots` for `**/SKILL.md`. Later roots don't override earlier same-named skills.
    pub fn discover(roots: &[PathBuf]) -> Self {
        let mut skills = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for root in roots {
            collect(root, 0, &mut skills, &mut seen);
        }
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        // Pre-render the solve-challenge orchestrator into its cached-prompt section once, so the
        // (unchanging) playbook isn't read from disk or re-rewritten on every turn.
        let preload_full = skills
            .iter()
            .find(|s| normalize(&s.name) == "solve-challenge")
            .and_then(|s| std::fs::read_to_string(s.dir.join("SKILL.md")).ok())
            .map(|body| render_preload_full(&body));
        Self { skills, preload_full }
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
    pub fn len(&self) -> usize {
        self.skills.len()
    }
    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }

    /// Compact catalog injected into the system prompt. Empty when no skills are installed.
    pub fn catalog_text(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }
        let lines = self
            .skills
            .iter()
            .map(|s| format!("- {}: {}", s.name, truncate(&s.description, DESC_CAP)))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            " INSTALLED SKILLS — proven, battle-tested playbooks. For ANY challenge that matches a \
             category below (web, binary/pwn, crypto, reverse, forensics, OSINT, etc.), call \
             use_skill with that name FIRST and follow its steps before improvising — it is the \
             fastest path to the flag:\n{lines}"
        )
    }

    /// Returns a formatted block suitable for direct injection into the stable (cached) system
    /// prompt, so the `solve-challenge` orchestrator workflow is ALWAYS active without the agent
    /// having to remember to invoke it. Empty when the skill is not installed.
    ///
    /// `slim` (small-context mode) emits only a compact workflow directive — the per-skill routing
    /// already lives in [`catalog_text`], so re-injecting the full ~2k-token body would duplicate it
    /// and blow the budget on an 8k window. Cloud mode gets the full body (cheap: it's cached).
    pub fn preloaded_system_section(&self, slim: bool) -> String {
        let Some(full) = &self.preload_full else { return String::new() };
        if slim {
            // Compact: rely on the always-present catalog for the per-skill keyword routing, and
            // just nail down the workflow + the two-level use_skill drill-down.
            return "\n CTF WORKFLOW (always active): (1) recon first — file/strings/nc/curl/binwalk \
                    the target; (2) identify the category and call use_skill(\"ctf-<category>\") from \
                    the skills list above; (3) call use_skill again with file=\"<technique>.md\" to get \
                    the exact steps, then FOLLOW them. Don't improvise before checking the matching skill."
                .to_string();
        }
        full.clone()
    }

    /// Find a skill by name (exact-normalized first, then a contains match).
    fn find(&self, name: &str) -> Option<&Skill> {
        let want = normalize(name);
        self.skills
            .iter()
            .find(|s| normalize(&s.name) == want)
            .or_else(|| self.skills.iter().find(|s| normalize(&s.name).contains(&want)))
    }

    /// Load a skill's `SKILL.md` (the router) plus a listing of its bundled files and how to load
    /// them. Name match is case-insensitive and tolerant of minor differences.
    pub fn load(&self, name: &str) -> Option<String> {
        let skill = self.find(name)?;
        let body = std::fs::read_to_string(skill.dir.join("SKILL.md")).ok()?;
        let files = list_bundled_files(&skill.dir);

        let body = sanitize_for_model(&body);
        // Two-level disclosure: the router lists technique files; the agent MUST pick one and
        // call use_skill again. DeepSeek needs this instruction at the TOP, before the body,
        // or it skims the router list and ignores it.
        let mut out = format!(
            "## HOW TO USE THIS SKILL\n\
             This is a ROUTER — it lists techniques below, not the steps themselves.\n\
             1. Read the technique list below and pick the ONE that matches your target.\n\
             2. Call use_skill AGAIN immediately with file=\"<that filename>\" (e.g. file=\"sql-injection.md\").\n\
             3. The second call loads the full step-by-step procedure. FOLLOW those steps.\n\
             Do NOT just read this list and move on — you MUST load a specific technique.\n\n\
             # Skill: {}\n{}\n",
            skill.name, truncate(&body, SKILL_BODY_CAP));
        if !files.is_empty() {
            out.push_str(&format!(
                "\n--- This skill routes to detailed technique files. To read the one you need, call \
                 use_skill AGAIN with file=\"<name>\" (e.g. use_skill name=\"{}\", \
                 file=\"{}\"), then follow its steps. Bundled files:\n{}",
                skill.name,
                files.first().map(String::as_str).unwrap_or("technique.md"),
                files.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")
            ));
        }
        Some(out)
    }

    /// Load a specific bundled file from a skill (full, capped), for the second level of disclosure.
    /// `file` is resolved strictly inside the skill directory (no traversal).
    pub fn load_file(&self, name: &str, file: &str) -> Option<String> {
        let skill = self.find(name)?;
        let rel = std::path::Path::new(file.trim());
        // Reject anything that could escape the skill dir.
        if rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::RootDir | std::path::Component::Prefix(_)))
        {
            return None;
        }
        let path = skill.dir.join(rel);
        let dir = skill.dir.canonicalize().ok()?;
        let canon = path.canonicalize().ok()?;
        if !canon.starts_with(&dir) {
            return None;
        }
        let content = std::fs::read_to_string(&canon).ok()?;
        Some(format!("# Skill {} — file: {}\n{}", skill.name, file, truncate(&sanitize_for_model(&content), SKILL_FILE_CAP)))
    }
}

/// Recursively look for `SKILL.md`. A directory that has one becomes a skill (we don't recurse into
/// it further — its subfolders are that skill's bundled assets).
fn collect(dir: &Path, depth: usize, out: &mut Vec<Skill>, seen: &mut HashSet<String>) {
    if depth > MAX_DEPTH || !dir.is_dir() {
        return;
    }
    let md = dir.join("SKILL.md");
    if md.is_file() {
        if let Ok(content) = std::fs::read_to_string(&md) {
            let (name, desc) = parse_frontmatter(&content);
            let name = name.unwrap_or_else(|| {
                dir.file_name().and_then(|n| n.to_str()).unwrap_or("skill").to_string()
            });
            let key = normalize(&name);
            if seen.insert(key) {
                out.push(Skill {
                    name,
                    description: desc.unwrap_or_else(|| "(no description)".to_string()),
                    dir: dir.to_path_buf(),
                });
            }
        }
        return; // don't descend into a skill's own asset folders
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "node_modules" || name == "tests" {
                continue;
            }
            collect(&path, depth + 1, out, seen);
        }
    }
}

/// Pull `name` and `description` out of leading `--- … ---` YAML frontmatter. Handles single-line
/// values and a folded/literal multi-line `description:` block (the common SKILL.md shapes) without
/// pulling in a full YAML dependency.
fn parse_frontmatter(content: &str) -> (Option<String>, Option<String>) {
    let trimmed = content.trim_start_matches('\u{feff}').trim_start();
    let Some(rest) = trimmed.strip_prefix("---") else { return (None, None) };
    let Some(end) = rest.find("\n---") else { return (None, None) };
    let fm = &rest[..end];

    let mut name = None;
    let mut description = None;
    let mut lines = fm.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(unquote(v.trim()));
        } else if let Some(v) = line.strip_prefix("description:") {
            let v = v.trim();
            if v == "|" || v == ">" || v.is_empty() {
                // Folded/literal block: gather subsequent indented lines.
                let mut buf = Vec::new();
                while let Some(next) = lines.peek() {
                    if next.starts_with(' ') || next.starts_with('\t') {
                        buf.push(next.trim().to_string());
                        lines.next();
                    } else {
                        break;
                    }
                }
                description = Some(buf.join(" "));
            } else {
                description = Some(unquote(v));
            }
        }
    }
    (name.filter(|s| !s.is_empty()), description.filter(|s| !s.is_empty()))
}

/// Build the full cached-prompt CTF-playbook section from a raw solve-challenge body: cap it,
/// rewrite slash commands to `use_skill(...)`, and wrap it in the mandatory-playbook header.
fn render_preload_full(body: &str) -> String {
    let translated = sanitize_for_model(&truncate(body, PRELOAD_BODY_CAP));
    format!(
        "\n\n## MANDATORY CTF PLAYBOOK (pre-loaded — follow before improvising)\n\
         The following orchestrator skill is always active. For ANY CTF challenge, follow \
         its workflow from Step 1. Do NOT skip it and invent your own approach — this skill \
         contains battle-tested categorisation and technique routing that is faster than \
         improvising. When it says to \"invoke\" a skill, call use_skill with that name.\n\n\
         {translated}"
    )
}

/// Rewrite `/ctf-*` and `/solve-challenge` slash commands to `use_skill("…")` calls so the
/// injected skill body is immediately actionable in this environment (no Claude Code slash
/// commands here). Leaves URLs (preceded by `:`) and other paths untouched.
fn rewrite_slash_commands(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(pos) = remaining.find('/') {
        let before = &remaining[..pos];
        let after = &remaining[pos + 1..];
        // Don't rewrite URL slashes (`:` immediately before) or empty names.
        if !before.ends_with(':') && after.starts_with(|c: char| c.is_ascii_alphabetic()) {
            let name_end = after
                .find(|c: char| !c.is_alphanumeric() && c != '-')
                .unwrap_or(after.len());
            let name = &after[..name_end];
            if name.starts_with("ctf-") || name == "solve-challenge" {
                result.push_str(before);
                result.push_str(&format!("use_skill(\"{name}\")"));
                remaining = &after[name_end..];
                continue;
            }
        }
        result.push_str(before);
        result.push('/');
        remaining = after;
    }
    result.push_str(remaining);
    result
}

fn list_bundled_files(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    walk_files(dir, dir, 0, &mut files);
    files.sort();
    files.truncate(60);
    files
}

fn walk_files(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk_files(root, &path, depth + 1, out);
        } else if name != "SKILL.md" {
            // Relative to the skill dir, so the agent can pass it straight to use_skill(file=…).
            if let Some(rel) = path.strip_prefix(root).ok().and_then(|p| p.to_str()) {
                out.push(rel.to_string());
            }
        }
    }
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').trim_matches('\'').to_string()
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s.to_string()
    }
}

/// Remove Claude-specific references from skill bodies so non-Claude models (DeepSeek, Ollama)
/// don't get confused or think the content isn't meant for them. Also rewrites `/ctf-*` slash
/// commands to `use_skill(…)` calls so the instructions are immediately actionable.
fn sanitize_for_model(text: &str) -> String {
    let text = text
        .replace("Claude Code or similar", "this environment")
        .replace("Claude Code", "this application")
        .replace("(Claude Code or similar)", "(this application)");
    rewrite_slash_commands(&text)
}

fn normalize(s: &str) -> String {
    s.trim().to_lowercase().replace([' ', '_'], "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_line_frontmatter() {
        let md = "---\nname: ctf-web\ndescription: Web exploitation — SQLi, XSS, SSTI.\n---\nbody here";
        let (name, desc) = parse_frontmatter(md);
        assert_eq!(name.as_deref(), Some("ctf-web"));
        assert_eq!(desc.as_deref(), Some("Web exploitation — SQLi, XSS, SSTI."));
    }

    #[test]
    fn parses_folded_description() {
        let md = "---\nname: ctf-pwn\ndescription: |\n  Binary exploitation.\n  ROP and heap.\n---\nbody";
        let (name, desc) = parse_frontmatter(md);
        assert_eq!(name.as_deref(), Some("ctf-pwn"));
        assert_eq!(desc.as_deref(), Some("Binary exploitation. ROP and heap."));
    }

    #[test]
    fn no_frontmatter_returns_none() {
        assert_eq!(parse_frontmatter("# just a heading\ntext"), (None, None));
    }

    #[test]
    fn discover_finds_skill_dirs() {
        let tmp = std::env::temp_dir().join(format!("tianji-skills-test-{}", std::process::id()));
        let skill_dir = tmp.join("ctf-web");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: ctf-web\ndescription: web stuff\n---\nhow to web",
        )
        .unwrap();
        std::fs::write(skill_dir.join("helper.py"), "print('hi')").unwrap();

        let cat = SkillCatalog::discover(&[tmp.clone()]);
        assert_eq!(cat.len(), 1);
        assert!(cat.catalog_text().contains("ctf-web: web stuff"));
        let loaded = cat.load("ctf-web").unwrap();
        assert!(loaded.contains("how to web"));
        assert!(loaded.contains("helper.py")); // bundled-file list
        // case/format tolerant
        assert!(cat.load("CTF_WEB").is_some());
        // second-level disclosure: load a specific bundled file in full
        let f = cat.load_file("ctf-web", "helper.py").unwrap();
        assert!(f.contains("print('hi')"));
        // path traversal is rejected
        assert!(cat.load_file("ctf-web", "../SKILL.md").is_none());
        assert!(cat.load_file("ctf-web", "nope.md").is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn slash_commands_rewritten_to_use_skill() {
        let input = "See /ctf-web for web, /ctf-pwn for pwn, /solve-challenge to start.\n\
                     URL https://example.com/path is untouched.\n\
                     Column: `/ctf-crypto`";
        let out = rewrite_slash_commands(input);
        assert!(out.contains("use_skill(\"ctf-web\")"), "ctf-web rewritten");
        assert!(out.contains("use_skill(\"ctf-pwn\")"), "ctf-pwn rewritten");
        assert!(out.contains("use_skill(\"solve-challenge\")"), "solve-challenge rewritten");
        assert!(out.contains("https://example.com/path"), "URL left intact");
    }

    #[test]
    fn preloaded_section_contains_skill_body() {
        let tmp = std::env::temp_dir().join(format!("tianji-sc-test-{}", std::process::id()));
        let sc_dir = tmp.join("solve-challenge");
        std::fs::create_dir_all(&sc_dir).unwrap();
        std::fs::write(
            sc_dir.join("SKILL.md"),
            "---\nname: solve-challenge\ndescription: orchestrator\n---\nStep 1: triage. Invoke /ctf-web for web.",
        ).unwrap();
        let cat = SkillCatalog::discover(&[tmp.clone()]);
        // Full (cloud) mode: full body, slash commands translated.
        let full = cat.preloaded_system_section(false);
        assert!(full.contains("MANDATORY CTF PLAYBOOK"), "header present");
        assert!(full.contains("Step 1: triage"), "body present");
        assert!(full.contains("use_skill(\"ctf-web\")"), "slash command translated");
        // Slim (8k local) mode: compact directive only, no full body (avoids blowing the window).
        let slim = cat.preloaded_system_section(true);
        assert!(slim.contains("CTF WORKFLOW"), "slim directive present");
        assert!(!slim.contains("Step 1: triage"), "slim must NOT carry the full body");
        assert!(slim.len() < full.len(), "slim section must be smaller");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
