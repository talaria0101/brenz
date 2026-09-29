//! Builtin script functions that run without a game attached.
//!
//! Everything here is pure: math in degrees (the engine converts with
//! pi/180, see `script_func_sin`), vector helpers with standard FPS
//! angle conventions, `isdefined`, and `print`/`println` captured into
//! the VM output buffer. Any engine builtin (entities, cvars, precache,
//! effects, objectives, ...) reports `BuiltinError::Game` naming it.

use super::value::Value;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// Error from a builtin call.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BuiltinError {
    /// Plain script error, like `Scr_Error`.
    Script(String),
    /// Needs a game world attached.
    Game(String),
}

/// Deterministic xorshift64 seed holder for `random*`.
pub(crate) struct Rng(pub u64);

impl Rng {
    pub(crate) fn next_u32(&mut self) -> u32 {
        let mut x = self.0.max(0x9e3779b97f4a7c15);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 32) as u32
    }
}

fn deg2rad(d: f32) -> f32 {
    d * std::f32::consts::PI / 180.0
}

fn rad2deg(r: f32) -> f32 {
    r * 180.0 / std::f32::consts::PI
}

fn num(n: &Value) -> Result<f32, BuiltinError> {
    match n {
        Value::Int(i) => Ok(*i as f32),
        Value::Float(f) => Ok(*f),
        v => Err(BuiltinError::Script(format!(
            "expected number, got {}",
            v.type_name()
        ))),
    }
}

fn vec(n: &Value) -> Result<[f32; 3], BuiltinError> {
    match n {
        Value::Vec3(v) => Ok(*v),
        v => Err(BuiltinError::Script(format!(
            "expected vector, got {}",
            v.type_name()
        ))),
    }
}

fn expect(args: &[Value], n: usize, name: &str) -> Result<(), BuiltinError> {
    if args.len() == n {
        Ok(())
    } else {
        Err(BuiltinError::Script(format!(
            "{name} expects {n} parameters, got {}",
            args.len()
        )))
    }
}

/// Lowercase names of engine builtin functions (from `builtins.ron`).
pub(crate) fn is_engine_function(name: &str) -> bool {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES
        .get_or_init(|| {
            #[derive(serde::Deserialize)]
            struct Builtins {
                functions: HashMap<String, serde::de::IgnoredAny>,
            }
            let m: Builtins =
                ron::from_str(include_str!("../assets/builtins.ron")).expect("builtins.ron");
            m.functions.into_keys().collect()
        })
        .contains(name)
}

/// Lowercase names of builtin methods (from `builtins.ron`).
pub(crate) fn is_engine_method(name: &str) -> bool {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES
        .get_or_init(|| {
            #[derive(serde::Deserialize)]
            struct Builtins {
                methods: HashMap<String, serde::de::IgnoredAny>,
            }
            let m: Builtins =
                ron::from_str(include_str!("../assets/builtins.ron")).expect("builtins.ron");
            m.methods.into_keys().collect()
        })
        .contains(name)
}

/// Certain parameter counts, `(min, max)`, from the C implementations.
/// Only what the engine provably enforces: strict `!=` checks, `<`
/// minimums, and the pure builtins above. Variadics (`print`, ...) and
/// anything optional-heavy report `None` and are never flagged.
pub(crate) fn arity(name: &str) -> Option<(usize, Option<usize>)> {
    let exact = |n: usize| Some((n, Some(n)));
    match name {
        "isdefined" | "assert" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "length"
        | "lengthsquared" | "vectornormalize" | "vectortoangles" | "anglestoforward"
        | "anglestoright" | "anglestoup" | "randomint" | "randomfloat" => exact(1),
        "distance" | "distancesquared" | "vectordot" | "randomintrange" | "randomfloatrange" => {
            exact(2)
        }
        "closer" => exact(3),
        "spawnstruct" => exact(0),
        "playfxontag" => exact(3),
        "rewindfx" => exact(2),
        "setcullfog" => exact(6),
        "setexpfog" => exact(5),
        "dodamage" => Some((2, None)),
        "dodamagemod" => Some((3, None)),
        _ => None,
    }
}

/// Whether `name` is implemented here (as opposed to needing a game
/// or being an unknown function).
pub(crate) fn is_pure(name: &str) -> bool {
    matches!(
        name,
        "isdefined" | "assert" | "print" | "logprint" | "println" | "sin" | "cos" | "tan"
            | "asin" | "acos" | "atan" | "distance" | "distancesquared" | "length"
            | "lengthsquared" | "vectordot" | "vectornormalize" | "vectortoangles"
            | "anglestoforward" | "anglestoright" | "anglestoup" | "closer" | "randomint"
            | "randomintrange" | "randomfloat" | "randomfloatrange" | "spawnstruct"
    )
}

/// Call a builtin by lowercase name. `out` collects `print` output.
pub(crate) fn call(
    name: &str,
    args: &[Value],
    rng: &mut Rng,
    out: &mut Vec<String>,
) -> Result<Value, BuiltinError> {
    use BuiltinError::*;
    match name {
        "isdefined" => {
            expect(args, 1, name)?;
            Ok(Value::Int(i32::from(args[0] != Value::Undefined)))
        }
        "assert" => {
            expect(args, 1, name)?;
            if args[0].truthy().map_err(Script)? {
                Ok(Value::Undefined)
            } else {
                Err(Script("assert failed".to_string()))
            }
        }
        "print" | "logprint" => {
            out.push(args.iter().map(|v| v.to_string()).collect());
            Ok(Value::Undefined)
        }
        "println" => {
            out.push(args.iter().map(|v| v.to_string()).collect());
            Ok(Value::Undefined)
        }
        "sin" => {
            expect(args, 1, name)?;
            Ok(Value::Float(deg2rad(num(&args[0])?).sin()))
        }
        "cos" => {
            expect(args, 1, name)?;
            Ok(Value::Float(deg2rad(num(&args[0])?).cos()))
        }
        "tan" => {
            expect(args, 1, name)?;
            Ok(Value::Float(deg2rad(num(&args[0])?).tan()))
        }
        "asin" => {
            expect(args, 1, name)?;
            Ok(Value::Float(rad2deg(num(&args[0])?.asin())))
        }
        "acos" => {
            expect(args, 1, name)?;
            Ok(Value::Float(rad2deg(num(&args[0])?.acos())))
        }
        "atan" => {
            expect(args, 1, name)?;
            Ok(Value::Float(rad2deg(num(&args[0])?.atan())))
        }
        "distance" => {
            expect(args, 2, name)?;
            let (a, b) = (vec(&args[0])?, vec(&args[1])?);
            let d = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
            Ok(Value::Float(d))
        }
        "distancesquared" => {
            expect(args, 2, name)?;
            let (a, b) = (vec(&args[0])?, vec(&args[1])?);
            Ok(Value::Float(
                (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2),
            ))
        }
        "length" => {
            expect(args, 1, name)?;
            let v = vec(&args[0])?;
            Ok(Value::Float((v[0].powi(2) + v[1].powi(2) + v[2].powi(2)).sqrt()))
        }
        "lengthsquared" => {
            expect(args, 1, name)?;
            let v = vec(&args[0])?;
            Ok(Value::Float(v[0].powi(2) + v[1].powi(2) + v[2].powi(2)))
        }
        "vectordot" => {
            expect(args, 2, name)?;
            let (a, b) = (vec(&args[0])?, vec(&args[1])?);
            Ok(Value::Float(a[0] * b[0] + a[1] * b[1] + a[2] * b[2]))
        }
        "vectornormalize" => {
            expect(args, 1, name)?;
            let v = vec(&args[0])?;
            let len = (v[0].powi(2) + v[1].powi(2) + v[2].powi(2)).sqrt().max(f32::EPSILON);
            Ok(Value::Vec3([v[0] / len, v[1] / len, v[2] / len]))
        }
        "vectortoangles" => {
            expect(args, 1, name)?;
            let v = vec(&args[0])?;
            let yaw = rad2deg(v[1].atan2(v[0]));
            let pitch = rad2deg((-v[2]).atan2((v[0].powi(2) + v[1].powi(2)).sqrt()));
            Ok(Value::Vec3([pitch, yaw, 0.0]))
        }
        "anglestoforward" => {
            expect(args, 1, name)?;
            let a = vec(&args[0])?;
            let (sp, cp) = deg2rad(a[0]).sin_cos();
            let (sy, cy) = deg2rad(a[1]).sin_cos();
            Ok(Value::Vec3([cp * cy, cp * sy, -sp]))
        }
        "anglestoright" => {
            expect(args, 1, name)?;
            let a = vec(&args[0])?;
            let (sy, cy) = deg2rad(a[1]).sin_cos();
            Ok(Value::Vec3([-sy, cy, 0.0]))
        }
        "anglestoup" => {
            expect(args, 1, name)?;
            let a = vec(&args[0])?;
            let (sp, cp) = deg2rad(a[0]).sin_cos();
            let (sy, cy) = deg2rad(a[1]).sin_cos();
            Ok(Value::Vec3([sp * cy, sp * sy, cp]))
        }
        "closer" => {
            expect(args, 3, name)?;
            let (a, b) = (vec(&args[0])?, vec(&args[1])?);
            let d = num(&args[2])?;
            let dist2 = (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2);
            Ok(Value::Int(i32::from(dist2 <= d * d)))
        }
        "randomint" => {
            expect(args, 1, name)?;
            let n = num(&args[0])? as i32;
            if n <= 0 {
                return Err(Script("randomint needs a positive bound".to_string()));
            }
            Ok(Value::Int((rng.next_u32() % (n as u32)) as i32))
        }
        "randomintrange" => {
            expect(args, 2, name)?;
            let (lo, hi) = (num(&args[0])? as i32, num(&args[1])? as i32);
            if hi <= lo {
                return Err(Script("randomintrange needs lo < hi".to_string()));
            }
            Ok(Value::Int(lo + (rng.next_u32() % ((hi - lo) as u32)) as i32))
        }
        "randomfloat" => {
            expect(args, 1, name)?;
            let n = num(&args[0])?;
            Ok(Value::Float(n * (rng.next_u32() as f32 / u32::MAX as f32)))
        }
        "randomfloatrange" => {
            expect(args, 2, name)?;
            let (lo, hi) = (num(&args[0])?, num(&args[1])?);
            Ok(Value::Float(lo + (hi - lo) * (rng.next_u32() as f32 / u32::MAX as f32)))
        }
        // A struct is just a field bag; no game state involved.
        "spawnstruct" => {
            expect(args, 0, name)?;
            Ok(Value::Struct(std::rc::Rc::new(std::cell::RefCell::new(
                std::collections::HashMap::new(),
            ))))
        }
        _ => Err(Game(format!("{name} needs a game attached"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(name: &str, args: Vec<Value>) -> Result<Value, BuiltinError> {
        call(name, &args, &mut Rng(1), &mut Vec::new())
    }

    #[test]
    fn trig_takes_degrees() {
        assert_eq!(run("sin", vec![Value::Int(90)]).unwrap(), Value::Float(1.0));
        let Value::Float(c) = run("cos", vec![Value::Int(90)]).unwrap() else {
            panic!()
        };
        assert!(c.abs() < 1e-6);
    }

    #[test]
    fn vectors_roundtrip() {
        let fwd = run("anglestoforward", vec![Value::Vec3([0.0, 90.0, 0.0])]).unwrap();
        let Value::Vec3(f) = fwd else { panic!() };
        assert!((f[0]).abs() < 1e-6 && (f[1] - 1.0).abs() < 1e-6 && f[2].abs() < 1e-6);
        let back = run("vectortoangles", vec![Value::Vec3([0.0, 1.0, 0.0])]).unwrap();
        let Value::Vec3(b) = back else { panic!() };
        assert!(b[0].abs() < 1e-4 && (b[1] - 90.0).abs() < 1e-4 && b[2].abs() < 1e-6);
    }

    #[test]
    fn game_builtins_report_game() {
        assert_eq!(
            run("getent", vec![Value::Str("x".into())]),
            Err(BuiltinError::Game("getent needs a game attached".to_string()))
        );
    }

    #[test]
    fn random_stays_in_range() {
        for _ in 0..50 {
            let Value::Int(n) = run("randomint", vec![Value::Int(10)]).unwrap() else {
                panic!()
            };
            assert!((0..10).contains(&n));
        }
    }
}
