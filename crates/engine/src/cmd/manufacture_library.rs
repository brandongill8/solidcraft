//! The saved tool library: tools kept in a file in the user's own folder, so a tool made once
//! can be added to any design (docs/manufacture.md). The design's own tools are in the design
//! (`manufacture.tool.set`); these commands copy a tool between the design and the file, by
//! name. A script can name another file with `path`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use solidcraft_manufacture as mf;

use super::CommandSpec;
use super::manufacture::{job_of, new_id, number_id, object, store, upsert};
use crate::params::bad;
use crate::{Result, Session};

/// The file's `format` value.
const FORMAT: &str = "solidcraft-tool-library/1";
/// The file in the user's data folder.
const FILE_NAME: &str = "tool-library.json";
/// Most tools a library holds.
const MAX_TOOLS: usize = 1000;
/// Largest library file read (bytes).
const MAX_BYTES: usize = 4 << 20;

pub static COMMANDS: &[CommandSpec] = &[
CommandSpec::new("manufacture.library.list", "List Saved Tools", list).icon("list").noundo().doc_only()
    .params("path?: another library file (default: tool-library.json in the SolidCraft data folder). Returns {path, tools}: the saved tools, which any design can add with `manufacture.library.load`. Does not change the design."),
CommandSpec::new("manufacture.library.save", "Save Tool to Library", save).icon("export").noundo().doc_only()
    .params("id: the design's tool number, name?: the name to save it under (default the tool's name; a saved tool of that name is replaced), path?. Returns {saved, path, count}. Does not change the design."),
CommandSpec::new("manufacture.library.load", "Add Saved Tool", load).icon("import").doc_only()
    .params("name: the saved tool, id?: the design's tool number to give it (default its saved number when free, else the next free one; an existing tool of that number is replaced), path?. One undo step."),
CommandSpec::new("manufacture.library.delete", "Delete Saved Tool", delete).icon("delete").noundo().doc_only()
    .params("name: the saved tool, path?. Does not change the design."),
];

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Library {
    format: String,
    tools: Vec<mf::Tool>,
}

impl Default for Library {
    fn default() -> Self {
        Library { format: FORMAT.into(), tools: Vec::new() }
    }
}

/// Where the library is: `path`, or the file in the user's data folder.
fn location(p: &Value, cmd: &str) -> Result<PathBuf> {
    let path = match p.get("path") {
        None | Some(Value::Null) => crate::recovery::data_dir()
            .map(|d| d.join(FILE_NAME))
            .ok_or_else(|| bad(cmd, "this system has no per-user folder to keep the library in: give a `path`"))?,
        Some(Value::String(path)) => {
            if path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control) {
                return Err(bad(cmd, "`path` must be a file path of at most 4096 characters"));
            }
            PathBuf::from(path)
        }
        Some(_) => return Err(bad(cmd, "`path` must be text")),
    };
    // A scoped session (an agent) reaches the library only where its scope allows.
    solidcraft_io::vfs::check(&path.to_string_lossy()).map_err(|e| bad(cmd, e.to_string()))?;
    Ok(path)
}

fn name_of(p: &Value, key: &str, cmd: &str) -> Result<String> {
    let n = p.get(key).and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty() && n.len() <= mf::tool::MAX_NAME_BYTES);
    n.map(str::to_owned).ok_or_else(|| bad(cmd, format!("`{key}` is a tool name of 1 to {} bytes", mf::tool::MAX_NAME_BYTES)))
}

/// The library file; a missing file is an empty library.
fn read(path: &Path, cmd: &str) -> Result<Library> {
    let name = path.to_string_lossy();
    if !solidcraft_io::vfs::exists(&name) {
        return Ok(Library::default());
    }
    let text = solidcraft_io::vfs::read_to_string(&name, MAX_BYTES).map_err(|e| bad(cmd, format!("could not read the tool library {name}: {e}")))?;
    let lib: Library = serde_json::from_str(&text).map_err(|e| bad(cmd, format!("the tool library {name} does not read: {e}")))?;
    if lib.format != FORMAT {
        return Err(bad(cmd, format!("the tool library {name} is `{}`, not `{FORMAT}`: it is from another version", lib.format)));
    }
    if lib.tools.len() > MAX_TOOLS {
        return Err(bad(cmd, format!("a tool library holds at most {MAX_TOOLS} tools")));
    }
    for t in &lib.tools {
        t.validate().map_err(|e| bad(cmd, format!("the saved tool `{}`: {e}", t.name)))?;
    }
    Ok(lib)
}

fn write(path: &Path, lib: &Library, cmd: &str) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(lib).map_err(|e| bad(cmd, e.to_string()))?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        solidcraft_io::vfs::create_dir_all(&dir.to_string_lossy())
            .map_err(|e| bad(cmd, format!("could not make the folder for the tool library: {e}")))?;
    }
    solidcraft_io::write_atomic(path, &bytes, false).map_err(|e| bad(cmd, format!("could not write the tool library: {e}")))
}

fn list(_s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "manufacture.library.list";
    object(p, cmd)?;
    let path = location(p, cmd)?;
    let lib = read(&path, cmd)?;
    Ok(json!({"path": path.to_string_lossy(), "tools": lib.tools}))
}

fn save(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "manufacture.library.save";
    object(p, cmd)?;
    let id = number_id(p.get("id").ok_or_else(|| bad(cmd, "`id` is required"))?, "id", cmd)?;
    let tool = s.doc.manufacture.tool(id).cloned().ok_or_else(|| bad(cmd, format!("there is no tool {id} in the design")))?;
    let path = location(p, cmd)?;
    let mut lib = read(&path, cmd)?;
    let name = if p.get("name").is_some() { name_of(p, "name", cmd)? } else { name_of(&json!({"name": tool.name}), "name", cmd)? };
    let tool = mf::Tool { name: name.clone(), ..tool };
    if !lib.tools.iter().any(|t| t.name == name) && lib.tools.len() >= MAX_TOOLS {
        return Err(bad(cmd, format!("a tool library holds at most {MAX_TOOLS} tools")));
    }
    upsert(&mut lib.tools, tool, |t| t.name == name);
    write(&path, &lib, cmd)?;
    Ok(json!({"saved": name, "path": path.to_string_lossy(), "count": lib.tools.len()}))
}

fn load(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "manufacture.library.load";
    object(p, cmd)?;
    let name = name_of(p, "name", cmd)?;
    let path = location(p, cmd)?;
    let lib = read(&path, cmd)?;
    let saved = lib.tools.into_iter().find(|t| t.name == name).ok_or_else(|| bad(cmd, format!("the library has no tool named `{name}`")))?;
    let mut job = job_of(s);
    let id = match p.get("id") {
        Some(v) => number_id(v, "id", cmd)?,
        None if job.tool(saved.id).is_none() => saved.id,
        None => new_id(cmd, job.tools.iter().map(|t| t.id))?,
    };
    let replaced = job.tool(id).is_some();
    let tool = mf::Tool { id, ..saved };
    tool.validate().map_err(|e| bad(cmd, e.to_string()))?;
    upsert(&mut job.tools, tool.clone(), |t| t.id == id);
    store(s, job, cmd)?;
    Ok(json!({"tool": tool, "created": !replaced}))
}

fn delete(_s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "manufacture.library.delete";
    object(p, cmd)?;
    let name = name_of(p, "name", cmd)?;
    let path = location(p, cmd)?;
    let mut lib = read(&path, cmd)?;
    if !lib.tools.iter().any(|t| t.name == name) {
        return Err(bad(cmd, format!("the library has no tool named `{name}`")));
    }
    lib.tools.retain(|t| t.name != name);
    write(&path, &lib, cmd)?;
    Ok(json!({"deleted": name, "path": path.to_string_lossy(), "count": lib.tools.len()}))
}

#[cfg(test)]
#[path = "manufacture_library_tests.rs"]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
