mod support;
use support::Case;

fn succeeds(source: &str, expected: &str) {
    let case = Case::new(source);
    let fast = case.run("");
    let plain = case.run_with_options("", &["--no-opt"]);
    assert!(
        fast.status.success(),
        "{}",
        String::from_utf8_lossy(&fast.stderr)
    );
    assert_eq!(fast.stdout, expected.as_bytes());
    assert_eq!(fast.stdout, plain.stdout);
    assert_eq!(fast.stderr, plain.stderr);
    assert_eq!(fast.status.code(), plain.status.code());
}

#[test]
fn captures_copy_update_and_rebind_follow_object_semantics() {
    succeeds(
        r#"
let counter:=1;
let bump:=(amount:Int)=>(counter+=amount);
let shared:=bump;let detached=bump;
wt bump(2),shared(3),counter; wt detached(10),counter;
counter:=100; wt bump(1),counter;
let n:=5;let rebind:=()=> (n:=n+1);wt rebind(),rebind(),n;
let text:="abc";let f:=()=>text[0];let c:=f();c='Z';wt text;
let stored:=[bump];stored[0](1);wt shared(0);
struct Box<F>{callback:F;}
fn Box<F>(callback:F)->Box<F>{return Box::<F>{callback:callback};}
let boxed:=Box(bump);wt boxed.callback(2);
"#,
        "3 6 6\n11 6\n7 100\n6 6 5\nZbc\n8\n10\n",
    );
}

#[test]
fn typed_lambda_parameters_named_functions_and_nested_generics_work() {
    succeeds(
        r#"
fn square(x:Int)->Int{return x*x;}
fn apply<T,F>(value:T,operation:F)->T{return operation(value);}
fn twice<T,F>(value:T,operation:F)->T{return apply(operation(value),operation);}
let f:=square;let same:=square;f:=same;wt f(3),twice(2,square);
let x:=100;let shadow:=(x:Int)=>x+1;wt shadow(3),x;
wt ((x:Int)=>x*2)(7);
let captured:="hi";let nested:=(x:Int)=>apply(x,(y:Int)=>y+captured.len());wt nested(3);
let plus:=(a:Int,b:Int)=>a+b;assert(3.plus(4)==7);let len:=0;assert([1,2].len()==2);
let count:=0;let unit:=(x:Int)=>assert(x>0);unit(1);
"#,
        "9 16\n4 100\n14\n5\n",
    );
}

#[test]
fn closures_keep_captured_character_owners_alive() {
    succeeds(
        r#"
fn use<F>(operation:F)->Char{return operation();}
let text:="abc";let c:=text[1];let f:=()=>c;
c:='Q';text:="new";wt use(f),text;
let detached=f;let held:=use(f);held='Z';wt use(f),use(detached),text;
"#,
        "b new\nZ b new\n",
    );
}

#[test]
fn callable_errors_are_rusk_diagnostics() {
    for source in [
        "let x:Int;let f:=()=>x;",
        "let f:=(x:Int,x:Int)=>x;",
        "let f:=(x:Int)=>x;wt f(true);",
        "let f:=(x:Int)=>x;wt f();",
        "let f:=1;f();",
        "fn f(x:Int)->Int{return x;} fn f(x:Int)->Int{return x;}let g:=f;g(1);",
        "let a:=(x:Int)=>x;let b:=(x:Int)=>x+1;a:=b;",
        "let x:=1;let f:=()=>x;wt f.x;",
        "let x:=1;let f:=()=>x;f.x:=2;",
        "let x:=1;let f:=()=>x;f.x=2;",
        "let x:=1;let f:=()=>x;(f.x,x):=(2,3);",
        "let f:=()=>missing;",
    ] {
        let case = Case::new(source);
        let out = case.compile();
        assert!(!out.status.success(), "{source}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("main.rusk:"),
            "{source}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!case.0.join("main.cpp").exists());
    }
}

#[test]
fn named_arguments_evaluate_explicit_values_before_defaults() {
    succeeds(
        r#"
fn score(trace: Vec<Int>, value: Int, *,
         a: Int = stamp(trace, value), b: Int,
         c: Int = stamp(trace, a + b), d: Int) -> Int {
    return a + b + c + d;
}
fn stamp(trace: Vec<Int>, value: Int) -> Int {
    trace.push_back(value);
    return value;
}
let trace: Vec<Int> := [];
wt score(trace, stamp(trace, 1), d: stamp(trace, 2), b: stamp(trace, 3));
wt trace;
trace.clear();
wt trace.score(stamp(trace, 1), c: stamp(trace, 9), b: stamp(trace, 3),
               a: stamp(trace, 5), d: stamp(trace, 2));
wt trace;
fn clip(values: Vec<Int>, *, end: Int = values.len()) -> Int { return end; }
fn source(trace: Vec<Int>) -> Vec<Int> { trace.push_back(0); return [1, 2]; }
trace.clear();
wt clip(source(trace)), trace;
"#,
        "10\n1 2 3 1 4\n19\n1 9 3 5 2\n2 0\n",
    );
}

#[test]
fn named_defaults_use_definition_captures_and_callback_types() {
    succeeds(
        r#"
fn apply<T, F>(value: T, *, callback: F) -> T { return callback(value); }
wt apply(4, callback: x => x + 3);
let use := apply;
wt use(4, callback: x => x + 4);

{
    let offset := 3;
    fn captured(base: Int, *, result: Int = base + offset) -> Int { return result; }
    fn generic<T>(base: T, *, result: T = base + offset) -> T { return result; }
    offset += 1;
    {
        let offset := 100;
        let use := captured;
        let poly := generic;
        wt captured(2), generic(2), captured(2, result: offset), use(3), poly(4);
    }
}
extern fn select_native<T>(x: T, *, value: T = x) -> T {
    code: "@{return T} @{fn}([[maybe_unused]] Obj<@{T}> x, Obj<@{T}> value, Loc) { return @{finish T}(std::move(value)); }",
}
wt select_native(8), 8.select_native(value: 10);

extern fn plain_native(*, value: Int = 6) -> Int {
    cpp_name: "b16_plain_native",
    code: "Obj<std::int64_t> b16_plain_native(Obj<std::int64_t> value, Loc) { return value; }",
}
fn choose(*, left: Int) -> Int { return left + 1; }
fn choose(*, right: Int) -> Int { return right + 2; }
wt plain_native(), plain_native(value: 9), choose(left: 5), choose(right: 5);
fn borrowed(noescape source: Int, *, other: Int = source) -> Int { return other + 1; }
wt borrowed(8);
"#,
        "7\n8\n6 6 100 7 8\n8 10\n6 9 6 7\n9\n",
    );
}

#[test]
fn named_call_diagnostics_report_argument_shape_and_ambiguity() {
    let declaration = "fn f(x: Int, *, start: Int) -> Int { return x + start; }";
    for (call, expected) in [
        ("f(1);", "missing required named argument 'start'"),
        ("f(1, stop: 2);", "unknown named argument 'stop'"),
        (
            "f(1, start: 2, start: 3);",
            "duplicate named argument 'start'",
        ),
        (
            "f(start: 2, 1);",
            "positional argument follows a named argument",
        ),
        ("f(1, 2);", "no matching function 'f'"),
        ("f(1, x: 2, start: 3);", "unknown named argument 'x'"),
    ] {
        let source = format!("{declaration}\n{call}");
        let case = Case::new(&source);
        for options in [&[][..], &["--no-opt"][..]] {
            let result = case.compile_with_options(options);
            assert!(!result.status.success());
            let diagnostic = String::from_utf8_lossy(&result.stderr);
            assert!(
                diagnostic.contains("main.rusk:2:") && diagnostic.contains(expected),
                "{diagnostic}"
            );
        }
    }

    let case = Case::new(
        r#"
fn f(x: Int, *, start: Int = 0) -> Int { return x + start; }
fn f(x: Int, *, stop: Int = 0) -> Int { return x + stop; }
wt f(1);
"#,
    );
    let result = case.compile();
    assert!(!result.status.success());
    let diagnostic = String::from_utf8_lossy(&result.stderr);
    assert!(diagnostic.contains("ambiguous"), "{diagnostic}");
}

#[test]
fn copying_a_function_keeps_its_default_and_body_captures_shared() {
    succeeds(
        r#"
{
    let count := 0;
    fn next(*, value: Int = (count += 1)) -> Int { return count + value; }
    let f := next;
    let g = f;
    wt f(), g(), f(), g(), count;
}
"#,
        "2 2 4 4 2\n",
    );
}
