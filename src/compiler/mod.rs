//! GSC compiler and interpreter, modeled on coduomp's script VM.
//!
//! `compile` lowers the tree-sitter AST to bytecode ([`Program`]); [`Vm`]
//! executes it with engine-like value semantics (see `value`).
//!
//! What runs without a game attached: values, variables, arithmetic,
//! control flow, local calls (including `::ref` pointers and struct
//! methods), arrays, structs, and the pure builtins in `builtin`.
//! Engine operations (`wait`, `thread`, entities, `level`, foreign
//! calls, ...) compile fine but raise a `needs a game` runtime error
//! if execution reaches them.

use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Range};
use tower_lsp_server as tower_lsp;

pub(crate) mod builtin;
pub(crate) mod compile;
pub(crate) mod op;
pub(crate) mod value;
pub(crate) mod vm;

#[derive(Debug, Clone)]
pub struct ScriptError {
    pub range: Range,
    pub message: String,
}

// Helper to convert SyntaxError to LSP Diagnostic
impl From<ScriptError> for Diagnostic {
    fn from(error: ScriptError) -> Self {
        Diagnostic {
            range: error.range,
            severity: Some(DiagnosticSeverity::ERROR),
            message: error.message,
            source: Some("CoD GSC".to_string()),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::compile::compile;
    use super::value;
    use super::vm::{DEFAULT_FUEL, RunResult, Vm};

    fn parse(src: &str) -> tree_sitter::Tree {
        let language = tree_sitter_gsc::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        parser.parse(src, None).unwrap()
    }

    pub(crate) fn run_src(src: &str) -> Result<RunResult, String> {
        let tree = parse(src);
        let program = compile(&tree, src).map_err(|errs| {
            errs.into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        let mut vm = Vm::new(&program, 0x1234, DEFAULT_FUEL);
        vm.run("main", Vec::new())
            .map_err(|e| format!("{}: {}", e.pos, e.message))
    }

    #[test]
    fn fib() {
        let out = run_src(
            "fib( n )\n{\n\tif ( n < 2 )\n\t\treturn n;\n\treturn fib( n - 1 ) + fib( n - 2 );\n}\nmain()\n{\n\treturn fib( 10 );\n}\n",
        )
        .unwrap();
        assert_eq!(out.value, value::Value::Int(55));
    }

    #[test]
    fn loops_arrays_structs() {
        let out = run_src(
            "main()\n{\n\ta = [];\n\tfor ( i = 0; i < 5; i++ )\n\t\ta[i] = i * i;\n\ttotal = 0;\n\tfor ( j = 0; j < a.size; j++ )\n\t\ttotal += a[j];\n\ts = spawnstruct();\n\ts.total = total;\n\treturn s.total;\n}\n",
        )
        .unwrap();
        assert_eq!(out.value, value::Value::Int(30));
    }

    #[test]
    fn duplicate_function_is_reported() {
        let src = "f()\n{\n\treturn 1;\n}\nf()\n{\n\treturn 2;\n}\nmain()\n{\n\treturn f();\n}\n";
        let tree = parse(src);
        let errs = compile(&tree, src).unwrap_err();
        assert!(
            errs.iter().any(|e| e.message.contains("already defined")),
            "{errs:?}"
        );
    }

    #[test]
    fn duplicate_case_is_reported() {
        let src = "main()\n{\n\tswitch ( x )\n\t{\n\t\tcase 1:\n\t\t\treturn 1;\n\t\tcase 1:\n\t\t\treturn 2;\n\t}\n}\n";
        let tree = parse(src);
        let errs = compile(&tree, src).unwrap_err();
        assert!(
            errs.iter().any(|e| e.message.contains("duplicate case")),
            "{errs:?}"
        );
    }

    #[test]
    fn foreach_is_rejected() {
        let src = "main()\n{\n\tforeach ( v in a )\n\t\tprintln( v );\n}\n";
        let tree = parse(src);
        let errs = compile(&tree, src).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("foreach is not supported")),
            "{errs:?}"
        );
    }

    #[test]
    fn switch_fallthrough() {
        let out = run_src(
            "main()\n{\n\tx = 1;\n\ty = 0;\n\tswitch ( x )\n\t{\n\t\tcase 1:\n\t\t\ty = 10;\n\t\tcase 2:\n\t\t\ty += 100;\n\t\t\tbreak;\n\t\tdefault:\n\t\t\ty = -1;\n\t}\n\treturn y;\n}\n",
        )
        .unwrap();
        assert_eq!(out.value, value::Value::Int(110));
    }

    #[test]
    fn switch_and_strings() {
        let out = run_src(
            "grade( x )\n{\n\tswitch ( x )\n\t{\n\t\tcase 1:\n\t\t\treturn \"one\";\n\t\tcase 2:\n\t\t\treturn \"two\";\n\t\tdefault:\n\t\t\treturn \"many\" + \"!\";\n\t}\n}\nmain()\n{\n\treturn grade( 7 );\n}\n",
        )
        .unwrap();
        assert_eq!(out.value, value::Value::Str("many!".into()));
    }

    #[test]
    fn array_assign_forms() {
        // Chained plain assignment yields the value.
        let out = run_src(
            "main()\n{\n\ta = [];\n\tb = [];\n\ta[0] = b[1] = 7;\n\treturn a[0] + b[1];\n}\n",
        )
        .unwrap();
        assert_eq!(out.value, value::Value::Int(14));
        // Compound assignment on elements.
        let out = run_src(
            "main()\n{\n\ta = [];\n\ta[0] = 3;\n\ta[0] += 4;\n\ta[0] *= 2;\n\treturn a[0];\n}\n",
        )
        .unwrap();
        assert_eq!(out.value, value::Value::Int(14));
        // Postfix on elements yields the old value.
        let out =
            run_src("main()\n{\n\ta = [];\n\ta[0] = 3;\n\tx = a[0]++;\n\treturn x + a[0];\n}\n")
                .unwrap();
        assert_eq!(out.value, value::Value::Int(7));
    }

    #[test]
    fn string_keyed_arrays() {
        let out = run_src(
            "main()\n{\n\ta = [];\n\ta[\"allies\"] = \"american\";\n\ta[0] = 1;\n\treturn a[\"allies\"];\n}\n",
        )
        .unwrap();
        assert_eq!(out.value, value::Value::Str("american".into()));
    }

    #[test]
    fn member_postfix() {
        // `obj.field++` yields old, stores new; game objects still
        // compile (they fail only if executed without a game).
        let out = run_src(
            "main()\n{\n\ts = spawnstruct();\n\ts.score = 10;\n\tx = s.score++;\n\ts.score--;\n\treturn x + s.score;\n}\n",
        )
        .unwrap();
        assert_eq!(out.value, value::Value::Int(20));
    }

    #[test]
    fn declared_strings_stay_neutral() {
        // A declared string return must never poison its context:
        // the runtime value decides, not the stand-in.
        check_ok("main()\n{\n\tif ( getcvar( \"x\" ) )\n\t\treturn 1;\n\treturn 0;\n}\n");
        check_ok("main()\n{\n\tx = getcvar( \"x\" ) + \"!\";\n\treturn x;\n}\n");
    }

    #[test]
    fn pointers_and_methods() {
        let out = run_src(
            "add( a, b )\n{\n\treturn a + b;\n}\nrun( f )\n{\n\treturn [[f]]( 20, 22 );\n}\napply( o, f )\n{\n\treturn o run( f );\n}\nmain()\n{\n\tf = ::add;\n\to = [];\n\treturn run( f ) + apply( o, f );\n}\n",
        )
        .unwrap();
        assert_eq!(out.value, value::Value::Int(84));
    }

    #[test]
    fn builtin_arity() {
        let tree = parse("main()\n{\n\tx = sin();\n}\n");
        let errs = compile(&tree, "main()\n{\n\tx = sin();\n}\n").unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("expects at least 1 parameters, got 0")),
            "{errs:?}"
        );
        let tree = parse("main()\n{\n\tx = sin( 30 );\n}\n");
        assert!(compile(&tree, "main()\n{\n\tx = sin( 30 );\n}\n").is_ok());
        // Extra arguments are ignored by the engine, never flagged.
        let tree = parse("main()\n{\n\tx = sin( 30, 40 );\n}\n");
        assert!(compile(&tree, "main()\n{\n\tx = sin( 30, 40 );\n}\n").is_ok());
        // Variadics are never flagged.
        let tree = parse("main()\n{\n\tprintln( \"a\", \"b\", 1 );\n}\n");
        assert!(compile(&tree, "main()\n{\n\tprintln( \"a\", \"b\", 1 );\n}\n").is_ok());
    }

    #[test]
    fn game_boundary_reports() {
        let err = run_src("main()\n{\n\twait 1;\n\treturn 0;\n}\n").unwrap_err();
        assert!(err.contains("needs a game scheduler"), "{err}");
        let err =
            run_src("main()\n{\n\tx = getent( \"a\", \"b\" );\n\treturn 0;\n}\n").unwrap_err();
        assert!(err.contains("needs a game attached"), "{err}");
    }

    #[test]
    fn while_break_continue() {
        let out = run_src(
            "main()\n{\n\ti = 0;\n\tsum = 0;\n\twhile ( true )\n\t{\n\t\ti++;\n\t\tif ( i % 2 == 0 )\n\t\t\tcontinue;\n\t\tif ( i > 7 )\n\t\t\tbreak;\n\t\tsum += i;\n\t}\n\treturn sum;\n}\n",
        )
        .unwrap();
        assert_eq!(out.value, value::Value::Int(16));
    }

    #[test]
    fn runtime_errors() {
        let err = run_src("main()\n{\n\treturn 1 / 0;\n}\n").unwrap_err();
        assert!(err.contains("divide by 0"), "{err}");
        let err = run_src("main()\n{\n\treturn nosuchfn();\n}\n").unwrap_err();
        assert!(err.contains("unknown function"), "{err}");
        let err =
            run_src("f( a, b )\n{\n\treturn a;\n}\nmain()\n{\n\treturn f( 7 );\n}\n").unwrap();
        assert_eq!(err.value, value::Value::Int(7));
    }

    fn check_ok(src: &str) {
        let tree = parse(src);
        assert!(compile(&tree, src).is_ok(), "{src}");
    }

    fn check_err(src: &str, want: &str) {
        let tree = parse(src);
        let errs = compile(&tree, src).unwrap_err();
        assert!(
            errs.iter().any(|e| e.message.contains(want)),
            "{want} not in {errs:?} for {src}"
        );
    }

    #[test]
    fn comments_everywhere_stay_quiet() {
        check_ok(
            "main()\n{\n\t// leading\n\tx = 1 /* inline */ + 2; // sum\n\tfoo( 1 /* one */, 2 );\n\t/* block */\n}\n",
        );
    }

    #[test]
    fn type_errors() {
        // Certain engine errors.
        check_err("main()\n{\n\tx = 1 + undefined;\n}\n", "unmatching types");
        check_err("main()\n{\n\tx = \"a\" - \"b\";\n}\n", "unmatching types");
        check_err("main()\n{\n\tx = ( 1, 2, 3 ) * 2;\n}\n", "unmatching types");
        check_err("main()\n{\n\tx = 1 / 0;\n}\n", "divide by 0");
        check_err("main()\n{\n\tx = 1.5 % 2;\n}\n", "unmatching types");
        check_err(
            "main()\n{\n\tif ( undefined )\n\t\treturn 1;\n}\n",
            "cannot cast undefined to bool",
        );
        check_err("main()\n{\n\tx = ~1.5;\n}\n", "cannot be applied");
        check_err("main()\n{\n\tx = (int)\"abc\";\n}\n", "cannot cast");
        check_err(
            "main()\n{\n\tx = ( 1, \"a\", 3 );\n}\n",
            "vector needs numbers",
        );
        check_err(
            "main()\n{\n\ta = [];\n\tx = a[( 1, 2, 3 )];\n}\n",
            "array index must be an integer or string",
        );
        check_ok("main()\n{\n\tgame[\"allies\"] = \"american\";\n}\n");
        check_err("main()\n{\n\tx = sin( \"abc\" );\n}\n", "expects float");
        check_err(
            "main()\n{\n\tx = distance( ( 0, 0, 0 ), 5 );\n}\n",
            "expects vector",
        );
        check_err(
            "main()\n{\n\tforeach ( v in 5 )\n\t\tprintln( v );\n}\n",
            "foreach needs an array",
        );
        // Legal code stays quiet.
        check_ok("main()\n{\n\tx = 1 + \"a\";\n}\n");
        check_ok("main()\n{\n\tx = undefined == undefined;\n}\n");
        check_ok("main()\n{\n\tx = sin( 30 );\n}\n");
        check_ok("main()\n{\n\tx = sin( \"30\" );\n}\n");
        check_ok("main()\n{\n\tx = 1 / 2;\n}\n");
        check_ok("main()\n{\n\tx = y + 1;\n}\n");
        check_ok("main()\n{\n\tif ( y )\n\t\treturn 1;\n}\n");
        check_ok("main()\n{\n\tx = getcvar( \"x\" ) + \"!\";\n}\n");
    }

    #[test]
    fn compile_errors_break() {
        let tree = parse("main()\n{\n\tbreak;\n}\n");
        let errs = compile(&tree, "main()\n{\n\tbreak;\n}\n").unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("break outside")));
    }

    #[test]
    fn array_size_field() {
        let out = run_src("main()\n{\n\ta = [];\n\ta[3] = 9;\n\treturn a.size;\n}\n").unwrap();
        assert_eq!(out.value, value::Value::Int(4));
    }

    #[test]
    fn print_capture_and_undefined() {
        let out = run_src(
            "main()\n{\n\tif ( !isdefined( missing ) )\n\t\tprintln( \"undef:\", missing );\n\treturn sin( 90 ) == 1;\n}\n",
        )
        .unwrap();
        assert_eq!(out.value, value::Value::Int(1));
        assert_eq!(out.output, vec!["undef:undefined"]);
    }
}
