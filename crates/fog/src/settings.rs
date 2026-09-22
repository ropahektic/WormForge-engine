//! The settings registry: a direct Rust port of wkFOW's `Settings::Def` table
//! (`wkFOW/src/Settings.cpp`), reduced to the Scheme scope the integration
//! covers. The same table drives Lua validation, defaults, and range errors,
//! so a setting's metadata lives in exactly one place — the reason wkFOW
//! built the registry in the first place.
//!
//! Scope is dropped: every setting here is gameplay (wkFOW's `Scope::Scheme`).
//! The three Client-scope performance knobs (`mask_resolution_divisor`,
//! `native_detail_radius`, `far_field_cell_size`) are engine constants in this
//! port, not Lua-exposed, because the two-tier LOD they tune is deferred.

/// Hard upper bound on any vision reach, in landscape pixels.
/// Mirrors `Config::MaxVisionDistance`.
pub const MAX_VISION_DISTANCE: i32 = 700;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Int,
    Bool,
    /// Small integer with named values; labels index from `min`.
    Enum(&'static [&'static str]),
}

#[derive(Clone, Copy, Debug)]
pub struct Def {
    /// snake_case identity, used as the Lua key.
    pub name: &'static str,
    pub description: &'static str,
    pub kind: Kind,
    pub default: i32,
    pub min: i32,
    pub max: i32,
    /// Safety-bounded quantities accept out-of-range values by clamping rather
    /// than erroring, matching wkFOW's `clampOutOfRange`.
    pub clamp: bool,
}

const DISCOVER_LABELS: &[&str] = &["off", "texture", "live", "premapped"];
const VIEW_OWNERSHIP_LABELS: &[&str] = &["alliance", "active", "everyone"];

/// The whole registry. Adding a setting means adding one row here.
pub const SETTINGS: &[Def] = &[
    Def {
        name: "circle_radius",
        description: "Awareness circle around worms, in pixels, independent of aim.",
        kind: Kind::Int,
        default: 250,
        min: 0,
        max: MAX_VISION_DISTANCE,
        clamp: true,
    },
    Def {
        name: "cone_width",
        description: "Width of the aim-directed view cone, in degrees. 360 is a full circle.",
        kind: Kind::Int,
        default: 45,
        min: 45,
        max: 360,
        clamp: false,
    },
    Def {
        name: "cone_length",
        description: "Reach of the aim-directed view cone, in pixels. 0 means no cone.",
        kind: Kind::Int,
        default: 0,
        min: 0,
        max: MAX_VISION_DISTANCE,
        clamp: true,
    },
    Def {
        name: "los",
        description: "Terrain blocks vision, for worms and animals alike. false reveals the full circle regardless of terrain.",
        kind: Kind::Bool,
        default: 1,
        min: 0,
        max: 1,
        clamp: false,
    },
    Def {
        name: "discover",
        description: "Terrain memory: 0 off, 1 stale terrain, 2 live terrain, 3 premapped.",
        kind: Kind::Enum(DISCOVER_LABELS),
        default: 1,
        min: 0,
        max: 3,
        clamp: false,
    },
    Def {
        name: "view_ownership",
        description: "Whose vision lights the map: 1 my alliance, 2 the active worm only, 3 everyone.",
        kind: Kind::Enum(VIEW_OWNERSHIP_LABELS),
        default: 1,
        min: 1,
        max: 3,
        clamp: false,
    },
    Def {
        name: "animal_circle_radius",
        description: "Awareness circle around every animal, in pixels. 0 means animals see nothing.",
        kind: Kind::Int,
        default: 150,
        min: 0,
        max: MAX_VISION_DISTANCE,
        clamp: true,
    },
    Def {
        name: "animal_cone_length",
        description: "Reach of an animal's view cone, in pixels. 0 means no cone.",
        kind: Kind::Int,
        default: 0,
        min: 0,
        max: MAX_VISION_DISTANCE,
        clamp: true,
    },
    Def {
        name: "animal_cone_width",
        description: "Width of an animal's view cone, in degrees. 360 is a full circle.",
        kind: Kind::Int,
        default: 45,
        min: 45,
        max: 360,
        clamp: false,
    },
    Def {
        name: "proj_circle",
        description: "Extra pixels of awareness outside each projectile's live sprite bounds.",
        kind: Kind::Int,
        default: 5,
        min: 0,
        max: MAX_VISION_DISTANCE,
        clamp: true,
    },
    Def {
        name: "proj_cone_length",
        description: "Reach of a projectile's view cone, in pixels. 0 means no cone.",
        kind: Kind::Int,
        default: 0,
        min: 0,
        max: MAX_VISION_DISTANCE,
        clamp: true,
    },
    Def {
        name: "proj_cone_width",
        description: "Width of a projectile's view cone, in degrees. 360 is a full circle.",
        kind: Kind::Int,
        default: 45,
        min: 45,
        max: 360,
        clamp: false,
    },
    Def {
        name: "proj_los",
        description: "Terrain blocks projectile vision.",
        kind: Kind::Bool,
        default: 0,
        min: 0,
        max: 1,
        clamp: false,
    },
    Def {
        name: "explosion_circle",
        description: "Temporary public light radius past a crater's rim after explosions.",
        kind: Kind::Int,
        default: 40,
        min: 0,
        max: MAX_VISION_DISTANCE,
        clamp: true,
    },
    Def {
        name: "crate_circle",
        description: "Terrain-blocked awareness circle around supply crates.",
        kind: Kind::Int,
        default: 20,
        min: 0,
        max: MAX_VISION_DISTANCE,
        clamp: true,
    },
    Def {
        name: "spectator_mode",
        description: "Raise the fog floor for eliminated players.",
        kind: Kind::Bool,
        default: 1,
        min: 0,
        max: 1,
        clamp: false,
    },
    Def {
        name: "placement_vision",
        description: "Map visible during manual worm placement.",
        kind: Kind::Bool,
        default: 0,
        min: 0,
        max: 1,
        clamp: false,
    },
    Def {
        name: "fog_floor",
        description: "Fog brightness floor, percent. 0 is full darkness; higher values lift the whole map toward visibility.",
        kind: Kind::Int,
        default: 0,
        min: 0,
        max: 100,
        clamp: true,
    },
];

pub fn find(name: &str) -> Option<&'static Def> {
    SETTINGS.iter().find(|d| d.name == name)
}

/// Validate one Lua-supplied value against its definition.
///
/// Returns the canonical integer to store. Bools accept Lua booleans or 0/1;
/// enums accept the label (case-insensitive) or the backing integer. Out-of-range
/// errors name the setting and its valid range, unless the definition opts to
/// clamp instead.
pub fn validate(def: &Def, value: &Value) -> Result<i32, String> {
    let raw: i32 = match (def.kind, value) {
        (Kind::Bool, Value::Bool(b)) => *b as i32,
        (_, Value::Int(v)) => *v,
        (Kind::Enum(labels), Value::Str(s)) => {
            let lower = s.to_ascii_lowercase();
            labels
                .iter()
                .position(|l| l.eq_ignore_ascii_case(&lower))
                .map(|i| def.min + i as i32)
                .ok_or_else(|| {
                    format!(
                        "'{s}' is not a valid {} (try {})",
                        def.name,
                        labels.join("/")
                    )
                })?
        }
        _ => {
            return Err(format!(
                "'{}' expects {}, got {}",
                def.name,
                kind_name(def.kind),
                value.type_name()
            ))
        }
    };
    if raw < def.min || raw > def.max {
        if def.clamp {
            return Ok(raw.clamp(def.min, def.max));
        }
        return Err(format!(
            "'{}' must be in {}..={}, got {}",
            def.name, def.min, def.max, raw
        ));
    }
    Ok(raw)
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Int => "an integer",
        Kind::Bool => "a boolean",
        Kind::Enum(_) => "an integer or label",
    }
}

/// A Lua-facing value, kept dependency-free so this crate never links mlua.
/// `lua-host` converts its `mlua::Value` into this before calling `validate`.
#[derive(Clone, Debug)]
pub enum Value {
    Int(i32),
    Bool(bool),
    Str(String),
}

impl Value {
    fn type_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "an integer",
            Value::Bool(_) => "a boolean",
            Value::Str(_) => "a string",
        }
    }
}

/// The resolved scheme: every setting's current value, keyed by registry index.
/// Defaults come straight from the registry so a fresh scheme is the registry's.
#[derive(Clone, Debug)]
pub struct Scheme {
    values: Vec<i32>,
}

impl Default for Scheme {
    fn default() -> Self {
        Scheme {
            values: SETTINGS.iter().map(|d| d.default).collect(),
        }
    }
}

impl Scheme {
    pub fn get(&self, name: &str) -> Option<i32> {
        let i = SETTINGS.iter().position(|d| d.name == name)?;
        Some(self.values[i])
    }

    pub fn get_def(&self, def: &Def) -> i32 {
        let i = SETTINGS.iter().position(|d| d.name == def.name).unwrap();
        self.values[i]
    }

    pub fn bool(&self, name: &str) -> bool {
        self.get(name).unwrap_or(0) != 0
    }

    /// Validate and store one setting. Returns the stored (possibly clamped) value.
    pub fn set(&mut self, name: &str, value: &Value) -> Result<i32, String> {
        let def = find(name).ok_or_else(|| unknown_setting(name))?;
        let v = validate(def, value)?;
        let i = SETTINGS.iter().position(|d| d.name == name).unwrap();
        self.values[i] = v;
        Ok(v)
    }
}

fn unknown_setting(name: &str) -> String {
    let mut names: Vec<&str> = SETTINGS.iter().map(|d| d.name).collect();
    names.sort_unstable();
    format!("unknown fog setting '{name}' (known: {})", names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_registry() {
        let s = Scheme::default();
        assert_eq!(s.get("circle_radius"), Some(250));
        assert_eq!(s.get("los"), Some(1));
        assert!(s.bool("los"));
        assert_eq!(s.get("view_ownership"), Some(1));
        assert_eq!(s.get("fog_floor"), Some(0));
    }

    #[test]
    fn validates_and_stores() {
        let mut s = Scheme::default();
        assert_eq!(s.set("circle_radius", &Value::Int(300)), Ok(300));
        assert_eq!(s.get("circle_radius"), Some(300));
        assert_eq!(s.set("los", &Value::Bool(false)), Ok(0));
        assert!(!s.bool("los"));
    }

    #[test]
    fn clamps_when_opted_in() {
        let mut s = Scheme::default();
        // circle_radius clamps out-of-range rather than erroring.
        assert_eq!(s.set("circle_radius", &Value::Int(9999)), Ok(700));
        assert_eq!(s.set("circle_radius", &Value::Int(-5)), Ok(0));
    }

    #[test]
    fn rejects_out_of_range_when_not_clamped() {
        let mut s = Scheme::default();
        assert!(s.set("cone_width", &Value::Int(10)).is_err());
        assert!(s.set("view_ownership", &Value::Int(0)).is_err());
    }

    #[test]
    fn enum_accepts_label_case_insensitively() {
        let mut s = Scheme::default();
        assert_eq!(s.set("discover", &Value::Str("Live".into())), Ok(2));
        assert_eq!(s.set("discover", &Value::Str("premapped".into())), Ok(3));
        assert!(s.set("discover", &Value::Str("bogus".into())).is_err());
    }

    #[test]
    fn unknown_setting_lists_known() {
        let mut s = Scheme::default();
        let e = s.set("nope", &Value::Int(1)).unwrap_err();
        assert!(e.contains("unknown fog setting 'nope'"));
        assert!(e.contains("circle_radius"));
    }
}
