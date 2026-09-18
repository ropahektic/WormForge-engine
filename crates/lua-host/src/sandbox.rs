use mlua::{Lua, Result as LuaResult};

pub fn new_lua() -> Result<Lua, String> {
    let lua = Lua::new();
    strip_unsafe(&lua).map_err(|e| e.to_string())?;
    Ok(lua)
}

fn strip_unsafe(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();
    for name in [
        "io",
        "os",
        "debug",
        "package",
        "dofile",
        "loadfile",
        "load",
        "require",
        "collectgarbage",
    ] {
        globals.set(name, mlua::Value::Nil)?;
    }
    // Lockstep: no host RNG. WA RNG is not wired yet; forbid math.random.
    let math: mlua::Table = globals.get("math")?;
    math.set("random", mlua::Value::Nil)?;
    math.set("randomseed", mlua::Value::Nil)?;
    Ok(())
}

pub fn run_script(lua: &Lua, src: &str, chunk_name: &str) -> Result<(), String> {
    lua.load(src)
        .set_name(chunk_name)
        .exec()
        .map_err(|e| e.to_string())
}
