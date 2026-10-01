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
        // Lockstep: coroutines suspend and resume script execution, and the
        // resume point is driven by whoever yields -- wall-clock input, hook
        // order, anything but the sim. A yield across a frame boundary would
        // also strand Rust-held Lua references across a GC cycle whose timing
        // differs per peer (gap 8). No example pack uses them; keep them out
        // rather than audit every yield path.
        "coroutine",
    ] {
        globals.set(name, mlua::Value::Nil)?;
    }
    // Lockstep: no host RNG. Lua seeds its own per process, so two peers would
    // draw different numbers from the first call. `wa.random()` replaces these
    // by drawing from WA's shared RNG, which both peers advance in step and a
    // replay reproduces exactly -- so these stay nil even now that one exists.
    let math: mlua::Table = globals.get("math")?;
    math.set("random", mlua::Value::Nil)?;
    math.set("randomseed", mlua::Value::Nil)?;
    Ok(())
}
