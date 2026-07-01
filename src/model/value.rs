//! Typed attribute values.

/// A typed feature-attribute value, mirroring the value types representable in a
/// Mapbox Vector Tile's key/value tables. Used for feature tags and, from
/// Phase 1, written into MVT layers by the encoder.
///
/// Floating-point variants deliberately keep `f32` and `f64` distinct: MVT
/// encodes them differently, and preserving the distinction keeps output at
/// parity with upstream. Because floats are not `Eq`/`Hash`, the encoder's
/// value-dedup table (Phase 1) hashes by bit pattern rather than deriving those
/// traits here.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// UTF-8 string.
    String(String),
    /// Boolean.
    Bool(bool),
    /// Signed 64-bit integer.
    Int(i64),
    /// Unsigned 64-bit integer.
    Uint(u64),
    /// 32-bit float.
    Float(f32),
    /// 64-bit float.
    Double(f64),
}

impl Value {
    /// The value as a string slice, if it is a [`Value::String`].
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::String(v.to_owned())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::String(v)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}
impl From<u64> for Value {
    fn from(v: u64) -> Self {
        Value::Uint(v)
    }
}
impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Value::Float(v)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Double(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_and_accessor() {
        assert_eq!(Value::from("hi"), Value::String("hi".to_owned()));
        assert_eq!(Value::from(true), Value::Bool(true));
        assert_eq!(Value::from(-3i64), Value::Int(-3));
        assert_eq!(Value::from(7u64), Value::Uint(7));
        assert_eq!(Value::from(1.5f32), Value::Float(1.5));
        assert_eq!(Value::from(1.5f64), Value::Double(1.5));

        assert_eq!(Value::from("x").as_str(), Some("x"));
        assert_eq!(Value::from(1i64).as_str(), None);
    }

    #[test]
    fn float_variants_are_distinct() {
        assert_ne!(Value::Float(1.0), Value::Double(1.0));
    }
}
