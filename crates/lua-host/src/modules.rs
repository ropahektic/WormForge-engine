//! Pack-local modules: `require("lib/shared")` inside a pack loads
//! `<pack>/lib/shared.lua`, and only from that pack.
//!
//! - A name is `/`-separated segments of letters, digits, `_` and `-`, with no
//!   extension; it can never leave the pack's folder. Symlinks are refused, as
//!   the network sync and the replay recorder refuse them, so a module is
//!   always a file that travelled with the pack.
//! - Each module runs once per match, in its own environment: it reads the
//!   shared globals (`wa`, the standard libraries, anything the pack's entry
//!   set) and its own `require`, but its global writes stay in the module.
//!   `require` returns what the module returned (`true` for nothing), the same
//!   value to every caller.
//! - A module that requires itself, directly or through others, is an error
//!   naming the chain; so is requiring the pack's entry file.
//!
//! The entry chunk keeps its old semantics: its global reads and writes go to
//! the shared globals, as every existing pack expects. Only `require` is its
//! own, so two packs with a `lib/util.lua` each load their own.
//!
//! Files are read when first required, from the pack's folder: during a match
//! that is the synchronised copy, during playback the copy the replay carried.

use mlua::{Lua, Table, Value};
use std::cell::RefCell;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

/// Longest module name accepted, in bytes.
const MAX_NAME: usize = 200;

/// Run a pack's entry chunk with the pack's own `require`.
pub fn run_entry(lua: &Lua, mod_id: &str, root: &Path, entry: &str, src: &str) -> Result<(), String> {
    let env = entry_env(lua, mod_id, root, entry).map_err(|e| e.to_string())?;
    lua.load(src)
        .set_name(entry)
        .set_environment(env)
        .exec()
        .map_err(|e| e.to_string())
}

/// The entry's environment: globals read and written through, `require` its own.
fn entry_env(lua: &Lua, mod_id: &str, root: &Path, entry: &str) -> mlua::Result<Table> {
    let env = lua.create_table()?;
    let meta = lua.create_table()?;
    meta.raw_set("__index", lua.globals())?;
    meta.raw_set("__newindex", lua.globals())?;
    env.set_metatable(Some(meta));

    let cache = lua.create_table()?;
    let loading: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let root = root.to_path_buf();
    let mod_id = mod_id.to_string();
    let entry = entry.replace('\\', "/");
    let pack_env = env.clone();
    let require = lua.create_function(move |lua, name: String| {
        let rel = module_path(&name).map_err(mlua::Error::external)?;
        if rel == entry {
            return Err(mlua::Error::external(format!(
                "require(\"{name}\"): {rel} is the pack's entry file"
            )));
        }
        if let Some(value) = cache.raw_get::<Option<Value>>(name.as_str())? {
            return Ok(value);
        }
        if loading.borrow().contains(&name) {
            let mut chain = loading.borrow().clone();
            chain.push(name.clone());
            return Err(mlua::Error::external(format!(
                "require cycle in {mod_id}: {}",
                chain.join(" -> ")
            )));
        }
        let src = read_module(&root, &rel).map_err(|e| {
            mlua::Error::external(format!("require(\"{name}\") in {mod_id}: {e}"))
        })?;

        // Private globals, falling back to the pack's environment (its
        // `require`, then the shared globals).
        let module_env = lua.create_table()?;
        let module_meta = lua.create_table()?;
        module_meta.raw_set("__index", pack_env.clone())?;
        module_env.set_metatable(Some(module_meta));

        loading.borrow_mut().push(name.clone());
        let result = lua
            .load(src)
            .set_name(rel.as_str())
            .set_environment(module_env)
            .eval::<Value>();
        loading.borrow_mut().pop();
        let value = match result? {
            Value::Nil => Value::Boolean(true),
            v => v,
        };
        cache.raw_set(name.as_str(), value.clone())?;
        Ok(value)
    })?;
    env.raw_set("require", require)?;
    Ok(env)
}

/// `lib/shared` -> `lib/shared.lua`, or why the name is not a module name.
fn module_path(name: &str) -> Result<String, String> {
    let bad = |why: &str| Err(format!("require(\"{name}\"): {why}"));
    if name.is_empty() || name.len() > MAX_NAME {
        return bad("a module name is 1 to 200 characters");
    }
    for segment in name.split('/') {
        if segment.is_empty() {
            return bad("empty path segment");
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return bad("use letters, digits, '_' and '-', with '/' between folders and no extension");
        }
    }
    Ok(format!("{name}.lua"))
}

/// The module's source, if it is a regular file inside the pack's folder.
fn read_module(root: &Path, rel: &str) -> Result<String, String> {
    let mut path = PathBuf::from(root);
    for part in Path::new(rel).components() {
        let Component::Normal(part) = part else {
            return Err(format!("{rel} is not a path inside the pack"));
        };
        path.push(part);
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() {
                return Err(format!("{rel} goes through a symlink"));
            }
        }
    }
    let meta = std::fs::metadata(&path).map_err(|_| format!("no {rel} in the pack"))?;
    if !meta.is_file() {
        return Err(format!("{rel} is not a file"));
    }
    std::fs::read_to_string(&path).map_err(|e| format!("{rel}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::module_path;

    #[test]
    fn module_names_stay_inside_the_pack() {
        assert_eq!(module_path("lib/shared").unwrap(), "lib/shared.lua");
        assert_eq!(module_path("weapons/javelin-2_b").unwrap(), "weapons/javelin-2_b.lua");
        for bad in ["", "../other/x", "/abs", "a//b", "a/", "a.b", "a\\b", "lib/shared.lua", "a b"] {
            assert!(module_path(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(module_path(&"a".repeat(201)).is_err());
    }
}
