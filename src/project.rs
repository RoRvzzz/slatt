use crate::{frontend, parser, rules::Script};
use rhai::AST;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub fn load(text: &str, path: &Path, label: &str) -> Result<(Script, Option<AST>), String> {
    let mut project = Project::default();
    let root = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    project.visit(&root, label, text)?;
    Ok((project.script, project.frontend))
}

#[derive(Default)]
struct Project {
    active: Vec<PathBuf>,
    loaded: BTreeSet<PathBuf>,
    pass_names: BTreeSet<String>,
    functions: BTreeSet<(String, usize, Option<String>)>,
    script: Script,
    frontend: Option<AST>,
}

impl Project {
    fn visit(&mut self, path: &Path, label: &str, text: &str) -> Result<(), String> {
        const MAX_INCLUDE_DEPTH: usize = 128;
        if self.active.len() == MAX_INCLUDE_DEPTH {
            return Err(format!(
                "{label}: include nesting exceeds {MAX_INCLUDE_DEPTH} files"
            ));
        }
        self.active.push(path.to_path_buf());
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut offset = 0;
        let mut padding = String::new();
        for (i, line) in text.split_inclusive('\n').enumerate() {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with("//") {
                // Blank or comment lines can appear between directives.
            } else if trimmed.strip_prefix("include").is_some_and(|rest| {
                rest.chars()
                    .next()
                    .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
            }) {
                let filename = parser::parse_include(line).map_err(|error| {
                    format!(
                        "{label}:{}:{}",
                        i + 1,
                        error.strip_prefix("1:").unwrap_or(&error)
                    )
                })?;
                let requested = path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(filename);
                let included = requested.canonicalize().map_err(|error| {
                    format!(
                        "{label}:{}:1: include {}: {error}",
                        i + 1,
                        requested.display()
                    )
                })?;
                if let Some(start) = self.active.iter().position(|parent| parent == &included) {
                    let chain = self.active[start..]
                        .iter()
                        .chain(std::iter::once(&included))
                        .map(|entry| entry.display().to_string())
                        .collect::<Vec<_>>()
                        .join(" -> ");
                    return Err(format!("{label}:{}:1: include cycle: {chain}", i + 1));
                }
                if !self.loaded.contains(&included) {
                    let included_label = included.display().to_string();
                    let source = fs::read_to_string(&included)
                        .map_err(|error| format!("{included_label}: {error}"))?;
                    self.visit(&included, &included_label, &source)?;
                }
            } else {
                break;
            }
            offset += line.len();
            if line.ends_with('\n') {
                padding.push('\n');
            }
        }
        let (script, ast) = frontend::parse(&(padding + &text[offset..]))
            .map_err(|error| format!("{label}:{error}"))?;
        for (name, map) in script.maps {
            if self.script.maps.insert(name.clone(), map).is_some() {
                return Err(format!("{label}: duplicate project map {name}"));
            }
        }
        for pass in script.passes {
            if !self.pass_names.insert(pass.name.clone()) {
                return Err(format!("{label}: duplicate project pass {}", pass.name));
            }
            self.script.passes.push(pass);
        }
        if let Some(ast) = ast {
            for function in ast.iter_functions() {
                if !self.functions.insert((
                    function.name.to_owned(),
                    function.params.len(),
                    function.this_type.map(str::to_owned),
                )) {
                    return Err(format!(
                        "{label}: duplicate project frontend function {}/{}",
                        function.name,
                        function.params.len()
                    ));
                }
            }
            if let Some(frontend) = &mut self.frontend {
                frontend.combine(ast);
            } else {
                self.frontend = Some(ast);
            }
        }
        self.active.pop();
        self.loaded.insert(path.to_path_buf());
        Ok(())
    }
}
