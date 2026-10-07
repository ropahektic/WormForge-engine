//! The only boundary between Lua pixels/factors and WA's signed 16.16 values.
use mlua::{Table, Value};

const SCALE: f64 = 65536.0;

/// Exact for every native i32, including its endpoints.
pub fn pixels(native: i32) -> f64 { native as f64 / SCALE }

/// Round ties away from zero, then check before casting (never saturate).
pub fn native(pixels: f64, field: &str) -> mlua::Result<i32> {
    let rounded = (pixels * SCALE).round();
    if !pixels.is_finite() || rounded < i32::MIN as f64 || rounded > i32::MAX as f64 {
        return Err(mlua::Error::external(format!("{field}: needs a finite pixel value representable as signed 16.16, got {pixels}")));
    }
    Ok(rounded as i32)
}

pub fn value(v: Value, field: &str) -> mlua::Result<i32> {
    match v {
        Value::Integer(n) => native(n as f64, field),
        Value::Number(n) => native(n, field),
        other => Err(mlua::Error::external(format!("{field}: needs a number in pixels (or a plain factor), got {other:?}"))),
    }
}

pub fn get(t: &Table, field: &str) -> mlua::Result<Option<i32>> {
    match t.get::<Value>(field)? {
        Value::Nil => Ok(None),
        v => value(v, field).map(Some),
    }
}

pub fn require(t: &Table, field: &str) -> mlua::Result<i32> {
    get(t, field)?.ok_or_else(|| mlua::Error::external(format!("{field}: required pixel value is missing")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_roundtrips_and_ties() {
        for v in [i32::MIN, i32::MAX, -65537, -1, 0, 1, 65536, 65537] {
            assert_eq!(native(pixels(v), "x").unwrap(), v);
        }
        assert_eq!(native(0.5 / SCALE, "x").unwrap(), 1);
        assert_eq!(native(-0.5 / SCALE, "x").unwrap(), -1);
        assert_eq!(native(-1.25, "vy").unwrap(), -81920);
        assert_eq!(value(Value::Integer(5), "x").unwrap(), 327680);
        assert_eq!(value(Value::Number(5.0), "x").unwrap(), 327680);
    }
    #[test]
    fn invalid_values_report_the_field_without_saturating() {
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 32768.0, -32768.0001,
                  (i32::MAX as f64 + 0.5) / SCALE, (i32::MIN as f64 - 0.5) / SCALE] {
            assert!(native(v, "warp.vx").unwrap_err().to_string().contains("warp.vx"));
        }
        assert!(value(Value::Boolean(true), "x").is_err());
    }
}
