//! Script values, modeled on coduomp's `VariableValue`.
//!
//! Coercion follows `CastWeakerPair`: the weaker side converts up
//! (`string` absorbs `float`/`vector`/`int`, `float` absorbs `int`).
//! Anything else is a `pair has unmatching types` error. Equality follows
//! `CheckEquality`, including `undefined == undefined` being true.
//! Arithmetic mirrors the VM's int/float/vector/string branches, with the
//! same `divide by 0` and `integer division overflow` errors.
//!
//! Approximations (engine string formatting internals are not observable
//! from here): float-to-string uses shortest round-trip text, vectors
//! print as `(x, y, z)`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

/// A runtime script value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Undefined,
    Int(i32),
    Float(f32),
    Str(String),
    IStr(String),
    Vec3([f32; 3]),
    /// Engine arrays are reference objects: assignment aliases.
    Array(Rc<RefCell<Vec<Value>>>),
    /// `spawnstruct` objects and field bags. Missing fields read
    /// as `undefined`, like the engine.
    Struct(Rc<RefCell<HashMap<String, Value>>>),
    /// `::name` function reference for `[[f]]()` calls.
    Func(String),
    /// Engine game object (entity and friends). Only used as a static
    /// stand-in for declared builtin signatures, never constructed
    /// by the VM itself.
    Entity,
}

impl Value {
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Value::Undefined => "undefined",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::IStr(_) => "localized string",
            Value::Vec3(_) => "vector",
            Value::Array(_) => "array",
            Value::Struct(_) => "struct",
            Value::Func(_) => "function",
            Value::Entity => "entity",
        }
    }

    /// Weaker type converts up, like `CastWeakerPair`.
    /// Returns the pair in a common type, or the engine's error text.
    pub(crate) fn coerce_pair(a: Value, b: Value) -> Result<(Value, Value), String> {
        use Value::*;
        use std::mem::discriminant;
        if discriminant(&a) == discriminant(&b) {
            return Ok((a, b));
        }
        match (a, b) {
            (Str(s), Int(i)) => Ok((Str(s), Str(fmt_int(i)))),
            (Int(i), Str(s)) => Ok((Str(fmt_int(i)), Str(s))),
            (Str(s), Float(f)) => Ok((Str(s), Str(fmt_float(f)))),
            (Float(f), Str(s)) => Ok((Str(fmt_float(f)), Str(s))),
            (Str(s), Vec3(v)) => Ok((Str(s), Str(fmt_vec(&v)))),
            (Vec3(v), Str(s)) => Ok((Str(fmt_vec(&v)), Str(s))),
            (Float(f), Int(i)) => Ok((Float(f), Float(i as f32))),
            (Int(i), Float(f)) => Ok((Float(i as f32), Float(f))),
            (l, r) => Err(format!(
                "pair has unmatching types '{}' and '{}'",
                l.type_name(),
                r.type_name()
            )),
        }
    }

    /// Equality after coercion, like `CheckEquality`.
    pub(crate) fn equals(a: &Value, b: &Value) -> Result<bool, String> {
        let (l, r) = Self::coerce_pair(a.clone(), b.clone())?;
        Ok(match (&l, &r) {
            (Value::Undefined, Value::Undefined) => true,
            (Value::Int(x), Value::Int(y)) => x == y,
            (Value::Float(x), Value::Float(y)) => x == y,
            (Value::Str(x), Value::Str(y)) => x == y,
            (Value::IStr(x), Value::IStr(y)) => x == y,
            (Value::Vec3(x), Value::Vec3(y)) => x == y,
            (Value::Array(x), Value::Array(y)) => x == y,
            (Value::Entity, Value::Entity) => true,
            _ => false,
        })
    }

    /// Truthiness: ints read directly, everything else goes through
    /// `CastBool` semantics. Returns the engine's error text on failure.
    pub(crate) fn truthy(&self) -> Result<bool, String> {
        match self {
            Value::Int(i) => Ok(*i != 0),
            Value::Float(f) => Ok(*f != 0.0),
            Value::Str(s) => {
                let n: i32 = s.trim().parse().unwrap_or(0);
                if n == 0 && !is_zero_literal(s) {
                    return Err(format!("cannot cast \"{s}\" to bool"));
                }
                Ok(n != 0)
            }
            v => Err(format!("cannot cast {} to bool", v.type_name())),
        }
    }

    /// `(int)` cast, like `CastInt`.
    pub(crate) fn cast_int(&self) -> Result<i32, String> {
        match self {
            Value::Int(i) => Ok(*i),
            Value::Float(f) => Ok(*f as i32),
            Value::Str(s) => {
                let n: i32 = s.trim().parse().unwrap_or(0);
                if n == 0 && !is_zero_literal(s) {
                    return Err(format!("cannot cast \"{s}\" to int"));
                }
                Ok(n)
            }
            v => Err(format!("cannot cast {} to int", v.type_name())),
        }
    }

    /// `(float)` cast, mirroring `CastFloat`.
    pub(crate) fn cast_float(&self) -> Result<f32, String> {
        match self {
            Value::Float(f) => Ok(*f),
            Value::Int(i) => Ok(*i as f32),
            Value::Str(s) => s
                .trim()
                .parse()
                .map_err(|_| format!("cannot cast \"{s}\" to float")),
            v => Err(format!("cannot cast {} to float", v.type_name())),
        }
    }

    /// `(string)` cast: strings pass through, numbers format.
    pub(crate) fn cast_string(&self) -> Result<String, String> {
        match self {
            Value::Str(s) | Value::IStr(s) => Ok(s.clone()),
            Value::Int(i) => Ok(fmt_int(*i)),
            Value::Float(f) => Ok(fmt_float(*f)),
            Value::Vec3(v) => Ok(fmt_vec(v)),
            v => Err(format!("cannot cast {} to string", v.type_name())),
        }
    }

    /// `(bool)` cast, like `CastBool` (0/1 int result).
    pub(crate) fn cast_bool(&self) -> Result<i32, String> {
        Ok(i32::from(self.truthy()?))
    }

    /// `a + b`, `a - b`, `a * b`, `a / b`, `a % b` after coercion.
    pub(crate) fn arith(op: char, a: &Value, b: &Value) -> Result<Value, String> {
        let (l, r) = Self::coerce_pair(a.clone(), b.clone())?;
        match (&l, &r) {
            (Value::Int(x), Value::Int(y)) => {
                let (x, y) = (*x, *y);
                Ok(Value::Int(match op {
                    '+' => x.wrapping_add(y),
                    '-' => x.wrapping_sub(y),
                    '*' => x.wrapping_mul(y),
                    '/' => {
                        if y == 0 {
                            return Err("divide by 0".to_string());
                        }
                        if x == i32::MIN && y == -1 {
                            return Err("integer division overflow".to_string());
                        }
                        x / y
                    }
                    '%' => {
                        if y == 0 {
                            return Err("divide by 0".to_string());
                        }
                        if x == i32::MIN && y == -1 {
                            return Err("integer division overflow".to_string());
                        }
                        x % y
                    }
                    _ => unreachable!(),
                }))
            }
            (Value::Float(x), Value::Float(y)) => {
                let (x, y) = (*x, *y);
                Ok(Value::Float(match op {
                    '+' => x + y,
                    '-' => x - y,
                    '*' => x * y,
                    '/' => {
                        if y == 0.0 && y == y {
                            return Err("divide by 0".to_string());
                        }
                        x / y
                    }
                    '%' => {
                        return Err(format!(
                            "pair has unmatching types '{}' and '{}'",
                            l.type_name(),
                            r.type_name()
                        ));
                    }
                    _ => unreachable!(),
                }))
            }
            (Value::Vec3(x), Value::Vec3(y)) if op == '+' || op == '-' => {
                let mut out = [0.0f32; 3];
                for i in 0..3 {
                    out[i] = if op == '+' { x[i] + y[i] } else { x[i] - y[i] };
                }
                Ok(Value::Vec3(out))
            }
            (Value::Str(x), Value::Str(y)) if op == '+' => {
                Ok(Value::Str(format!("{x}{y}")))
            }
            _ => Err(format!(
                "pair has unmatching types '{}' and '{}'",
                l.type_name(),
                r.type_name()
            )),
        }
    }

    /// `<`, `>`, `<=`, `>=` after coercion, on ints, floats and strings.
    pub(crate) fn compare(op: &str, a: &Value, b: &Value) -> Result<bool, String> {
        let (l, r) = Self::coerce_pair(a.clone(), b.clone())?;
        let ord = match (&l, &r) {
            (Value::Int(x), Value::Int(y)) => x.cmp(y),
            (Value::Float(x), Value::Float(y)) => x
                .partial_cmp(y)
                .ok_or_else(|| "comparison on NaN".to_string())?,
            (Value::Str(x), Value::Str(y)) => x.cmp(y),
            _ => {
                return Err(format!(
                    "pair has unmatching types '{}' and '{}'",
                    l.type_name(),
                    r.type_name()
                ));
            }
        };
        use std::cmp::Ordering::*;
        Ok(match op {
            "<" => ord == Less,
            ">" => ord == Greater,
            "<=" => ord != Greater,
            ">=" => ord != Less,
            _ => unreachable!(),
        })
    }

    /// `size`, like `GetSizeValue`: arrays count, strings byte length.
    pub(crate) fn size(&self) -> Result<i32, String> {
        match self {
            Value::Array(items) => Ok(items.borrow().len() as i32),
            Value::Str(s) => Ok(s.len() as i32),
            v => Err(format!("size cannot be applied to {}", v.type_name())),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Undefined => write!(f, "undefined"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{}", fmt_float(*x)),
            Value::Str(s) | Value::IStr(s) => write!(f, "{s}"),
            Value::Vec3(v) => write!(f, "{}", fmt_vec(v)),
            Value::Array(items) => {
                write!(f, "[")?;
                for (i, item) in items.borrow().iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
            Value::Struct(fields) => {
                let fields = fields.borrow();
                let mut keys: Vec<&String> = fields.keys().collect();
                keys.sort();
                write!(f, "{{")?;
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {}", fields[*k])?;
                }
                write!(f, "}}")
            }
            Value::Func(name) => write!(f, "::{name}"),
            Value::Entity => write!(f, "entity"),
        }
    }
}

fn fmt_int(i: i32) -> String {
    i.to_string()
}

fn fmt_float(f: f32) -> String {
    if f == f.trunc() && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

fn fmt_vec(v: &[f32; 3]) -> String {
    format!("({}, {}, {})", fmt_float(v[0]), fmt_float(v[1]), fmt_float(v[2]))
}

/// Mirrors the engine's zero-literal check: `atoi` yielding 0 is only a
/// valid cast when the text actually spells zero.
pub(crate) fn is_zero_literal(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.bytes().all(|b| b == b'0' || b == b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_arith_wraps_and_guards() {
        assert_eq!(
            Value::arith('+', &Value::Int(i32::MAX), &Value::Int(1)).unwrap(),
            Value::Int(i32::MIN)
        );
        assert!(Value::arith('/', &Value::Int(1), &Value::Int(0)).is_err());
        assert!(Value::arith('/', &Value::Int(i32::MIN), &Value::Int(-1)).is_err());
    }

    #[test]
    fn coercion_rules() {
        let (l, r) = Value::coerce_pair(Value::Str("n: ".into()), Value::Int(7)).unwrap();
        assert_eq!((l, r), (Value::Str("n: ".into()), Value::Str("7".into())));
        let (l, r) = Value::coerce_pair(Value::Int(1), Value::Float(0.5)).unwrap();
        assert_eq!((l, r), (Value::Float(1.0), Value::Float(0.5)));
        assert!(Value::coerce_pair(Value::Int(1), Value::Vec3([0.0; 3])).is_err());
    }

    #[test]
    fn undefined_equals_undefined() {
        assert!(Value::equals(&Value::Undefined, &Value::Undefined).unwrap());
        assert!(Value::equals(&Value::Int(0), &Value::Undefined).is_err());
    }

    #[test]
    fn truthiness_matches_cast_bool() {
        assert!(Value::Int(3).truthy().unwrap());
        assert!(!Value::Int(0).truthy().unwrap());
        assert!(Value::Float(0.5).truthy().unwrap());
        assert!(Value::Str("0".into()).truthy().unwrap() == false);
        assert!(Value::Str("abc".into()).truthy().is_err());
        assert!(Value::Undefined.truthy().is_err());
        assert!(Value::Vec3([1.0, 0.0, 0.0]).truthy().is_err());
    }

    #[test]
    fn string_concat_and_vector_math() {
        assert_eq!(
            Value::arith('+', &Value::Str("a".into()), &Value::Int(1)).unwrap(),
            Value::Str("a1".into())
        );
        assert_eq!(
            Value::arith(
                '-',
                &Value::Vec3([3.0, 2.0, 1.0]),
                &Value::Vec3([1.0, 1.0, 1.0])
            )
            .unwrap(),
            Value::Vec3([2.0, 1.0, 0.0])
        );
        assert!(Value::arith('*', &Value::Vec3([1.0; 3]), &Value::Float(2.0)).is_err());
    }
}
