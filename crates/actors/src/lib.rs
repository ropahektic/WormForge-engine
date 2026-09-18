//! Namespaced LuaActor kinds (`mod_id.kind`).
//!
//! Custom bodies share **one** native type (vtable + CollisionManager
//! register). Mods do not stack overrides on stock `CWorm` / `CGObject`.
//!
//! Lua names the steps (cursor, spawn, sprite, loop, origin, explode).
//! Native code runs those primitives; it does not ship a hidden weapon kind.

/// `mod_id.kind`. If `kind` is already `mod_id.`-prefixed, it is returned as-is.
pub fn kind_id(mod_id: &str, kind: &str) -> String {
    let kind = kind.trim();
    if kind.starts_with(mod_id) && kind.as_bytes().get(mod_id.len()) == Some(&b'.') {
        kind.to_string()
    } else {
        format!("{mod_id}.{kind}")
    }
}

/// Concrete Donkey / airstrike click-drop, implemented natively.
pub const NATIVE_DROP: &str = "drop";

pub fn is_drop(kind: &str) -> bool {
    let k = kind.trim();
    let tail = k.rsplit('.').next().unwrap_or(k);
    tail.eq_ignore_ascii_case(NATIVE_DROP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaced_kind() {
        assert_eq!(kind_id("example.pack", "boss_mole"), "example.pack.boss_mole");
        assert_eq!(
            kind_id("example.pack", "example.pack.boss_mole"),
            "example.pack.boss_mole"
        );
    }

    #[test]
    fn drop_kind_matches_short_and_namespaced() {
        assert!(is_drop("drop"));
        assert!(is_drop("example.gif_donkey.drop"));
        assert!(!is_drop("boss_mole"));
    }
}
