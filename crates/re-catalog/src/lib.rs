//! Loads the `re/*.toml` catalog. Addresses are Ghidra VAs at image base 0x400000.

use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub functions: HashMap<String, Function>,
    pub globals: HashMap<String, Global>,
    pub structs: HashMap<String, Struct>,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub va: u32,
    pub name: String,
    pub calling_convention: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Global {
    pub va: u32,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Struct {
    pub name: String,
    pub size: Option<u32>,
    pub fields: Vec<StructField>,
}

#[derive(Debug, Clone)]
pub struct StructField {
    pub offset: u32,
    pub name: Option<String>,
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    function: Vec<RawFunction>,
    #[serde(default)]
    global: Vec<RawGlobal>,
    #[serde(default, rename = "struct")]
    structs: Vec<RawStruct>,
}

#[derive(Deserialize)]
struct RawFunction {
    va: u32,
    name: String,
    #[serde(default)]
    calling_convention: Option<String>,
}

#[derive(Deserialize)]
struct RawGlobal {
    va: u32,
    name: String,
}

#[derive(Deserialize)]
struct RawStruct {
    name: String,
    #[serde(default)]
    size: Option<u32>,
    #[serde(default)]
    field: Vec<RawField>,
}

#[derive(Deserialize)]
struct RawField {
    offset: u32,
    #[serde(default)]
    name: Option<String>,
}

impl Catalog {
    pub fn parse_toml(src: &str) -> Result<Self, String> {
        let mut c = Self::default();
        c.merge_toml(src)?;
        Ok(c)
    }

    pub fn merge_toml(&mut self, src: &str) -> Result<(), String> {
        let file: File = toml::from_str(src).map_err(|e| e.to_string())?;
        for f in file.function {
            self.functions.insert(
                f.name.clone(),
                Function {
                    va: f.va,
                    name: f.name,
                    calling_convention: f.calling_convention,
                },
            );
        }
        for g in file.global {
            self.globals.insert(
                g.name.clone(),
                Global {
                    va: g.va,
                    name: g.name,
                },
            );
        }
        for s in file.structs {
            self.structs.insert(
                s.name.clone(),
                Struct {
                    name: s.name,
                    size: s.size,
                    fields: s
                        .field
                        .into_iter()
                        .map(|f| StructField {
                            offset: f.offset,
                            name: f.name,
                        })
                        .collect(),
                },
            );
        }
        Ok(())
    }

    pub fn builtin() -> Result<Self, String> {
        let mut c = Self::default();
        c.merge_toml(include_str!("../../../re/InitWeaponTable.toml"))?;
        c.merge_toml(include_str!("../../../re/GameRuntime.toml"))?;
        c.merge_toml(include_str!("../../../re/GameWorld.toml"))?;
        c.merge_toml(include_str!("../../../re/WeaponEntry.toml"))?;
        c.merge_toml(include_str!("../../../re/globals.toml"))?;
        c.merge_toml(include_str!("../../../re/CollisionManager.toml"))?;
        c.merge_toml(include_str!("../../../re/MissileEntity.toml"))?;
        c.merge_toml(include_str!("../../../re/WormEntity.toml"))?;
        c.merge_toml(include_str!("../../../re/Hud.toml"))?;
        c.merge_toml(include_str!("../../../re/FireWeapon.toml"))?;
        c.merge_toml(include_str!("../../../re/hurt.toml"))?;
        Ok(c)
    }

    pub fn function_va(&self, name: &str) -> Option<u32> {
        self.functions.get(name).map(|f| f.va)
    }

    pub fn global_va(&self, name: &str) -> Option<u32> {
        self.globals.get(name).map(|g| g.va)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct HookDecl {
    pub wa_function: String,
    pub action: String,
}

#[derive(Deserialize)]
struct HookFile {
    #[serde(default)]
    hook: Vec<HookDecl>,
}

pub fn parse_hooks(src: &str) -> Result<Vec<HookDecl>, String> {
    let file: HookFile = toml::from_str(src).map_err(|e| e.to_string())?;
    Ok(file.hook)
}

pub fn builtin_hooks() -> Result<Vec<HookDecl>, String> {
    parse_hooks(include_str!("../../../hooks/weapon.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_weapon_table_va() {
        let c = Catalog::builtin().expect("catalog");
        assert_eq!(c.function_va("InitWeaponTable"), Some(0x0053CAB0));
        assert_eq!(c.global_va("STR_VERSION_381"), Some(0x00641C60));
        let world = c.structs.get("GameWorld").unwrap();
        let wt = world.fields.iter().find(|f| f.name.as_deref() == Some("weapon_table")).unwrap();
        assert_eq!(wt.offset, 0x510);
        let cm = c.structs.get("CollisionManager").unwrap();
        assert_eq!(cm.size, Some(0x264));
        let wind = cm.fields.iter().find(|f| f.name.as_deref() == Some("wind_bias")).unwrap();
        assert_eq!(wind.offset, 0x230);
        assert_eq!(
            c.function_va("CollisionManager__apply_projectile_gravity"),
            Some(0x004FCB30)
        );
        let world = c.structs.get("GameWorld").unwrap();
        let col = world
            .fields
            .iter()
            .find(|f| f.name.as_deref() == Some("collision_manager"))
            .unwrap();
        assert_eq!(col.offset, 0x528);
        assert_eq!(
            c.function_va("FireWeapon__CreateWeaponProjectile"),
            Some(0x0051E0F0)
        );
        assert_eq!(
            c.function_va("FireWeapon__StrikeFire"),
            Some(0x0051E2C0)
        );
    }

    #[test]
    fn wrap_init_weapon_table_hook() {
        let hooks = builtin_hooks().unwrap();
        assert_eq!(hooks[0].wa_function, "InitWeaponTable");
        assert_eq!(hooks[0].action, "wrap_after");
        assert_eq!(hooks[1].wa_function, "GfxHandler__LoadSprites");
        assert_eq!(hooks[2].wa_function, "CollisionManager__apply_projectile_gravity");
        assert_eq!(hooks.len(), 8);
    }
}
