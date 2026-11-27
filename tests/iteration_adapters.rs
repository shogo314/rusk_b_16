mod support;
use support::Case;

fn runs(source: &str, expected: &str) {
    let case = Case::new(source);
    for options in [&[][..], &["--no-opt"][..]] {
        let output = if options.is_empty() {
            case.run("")
        } else {
            case.run_with_options("", options)
        };
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, expected.as_bytes());
    }
}

#[test]
fn adapters_share_values_and_copy_positions() {
    runs(
        include_str!("../examples/beta09/iteration_adapters.rusk"),
        "3 6 9\n9 10\n6 11 3 12 9 10 6 11 3 12\nabX! 5 4 3\n",
    );
}

#[test]
fn zip_checks_left_then_right_and_caches_readiness() {
    runs(
        r#"
struct Cursor { index: Int; events: Vec<Int>; tag: Int; }
fn __iter__(it: Cursor) -> Cursor { return it; }
fn __iter_has_next__(it: Cursor) -> Bool {
    it.events.push_back(it.tag);
    return it.index < 2;
}
fn __iter_next__(it: Cursor) -> Int {
    let result = it.index;
    it.index += 1;
    return result;
}
let events: Vec<Int> := [];
let left := Cursor { index: 0, events: events, tag: 1 };
let right := Cursor { index: 0, events: events, tag: 2 };
let pairs := zip(left, right);
wt __iter_has_next__(pairs), __iter_has_next__(pairs), events;
wt __iter_next__(pairs), events;
wt pairs.to_vec(), events;
"#,
        "Yes Yes 1 2\n0 0 1 2\n1 1 1 2 1 2 1 2\n",
    );
}

#[test]
fn adapters_compose_with_partially_consumed_and_short_circuiting_sources() {
    runs(
        r#"
let calls := 0;
let source := (0 .. 5).filter((x: Int) => x % 2 == 0).map((x: Int) => x + (calls += 1) * 0);
for value in source { wt value; break; }
let cursor := source.enumerate(-1);
wt calls;
let detached = cursor;
wt cursor.to_vec(), calls;
wt detached.to_vec(), calls;
let fewer := zip(0 .. 1, 2 .. 5);
wt fewer.any((pair: (Int, Int)) => pair[0] == 0);
let direct := __iter__([4, 5, 6]);
wt __iter_next__(direct), direct.enumerate().to_vec();
let range := (-9223372036854775808 .. -9223372036854775807).reversed();
wt range.to_vec();
let units: Vec<Unit> := [{}, {}];
wt units.enumerate().to_vec().len(), zip(units, [1, 2]).to_vec().len();
"#,
        "0\n1\n-1 2 0 4 3\n-1 2 0 4 3\nYes\n4 0 5 1 6\n-9223372036854775808\n2 2\n",
    );
}

#[test]
fn readonly_vector_iteration_advances_cursors_and_preserves_element_aliases() {
    runs(
        r#"
fn read(readonly noescape values: Vec<Int>) {
    for value in values { wt value; }
    for (index, value) in values.enumerate() { wt index, value; }
    for (left, right) in zip(values, values) { wt left, right; }
    let cursor := values.enumerate();
    let shared := cursor;
    let detached = cursor;
    wt __iter_next__(cursor), __iter_next__(shared), __iter_next__(detached);
}
read([4, 7]);
"#,
        "4\n7\n0 4\n1 7\n4 4\n7 7\n0 4 1 7 0 4\n",
    );
    for source in [
        "readonly let a := [1]; for value in a { let alias := value; alias = 2; }",
        "readonly let a := [1]; for (i, value) in enumerate(a) { value = 2; }",
        "readonly let a := [[1]]; for (left, right) in zip([0], a) { let alias := right[0]; alias = 2; }",
        "readonly let a := [1]; for value in a { a.push_back(2); }",
    ] {
        for options in [&[][..], &["--no-opt"][..]] {
            let output = Case::new(source).compile_with_options(options);
            assert_eq!(output.status.code(), Some(1), "{source}");
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(
                error.contains("cannot modify readonly access") && error.contains("main.rusk:"),
                "{error}"
            );
        }
    }
}

#[test]
fn readonly_vector_map_tracks_callback_input_and_result_separately() {
    runs(
        r#"
fn read(readonly noescape values: Vec<Int>) {
    for (index, value) in values.map((readonly x: Int) => x + 1).enumerate() {
        value += 1;
        wt index, value;
    }
    for value in values.map((readonly x: Int) => x) { wt value; }
}
read([4, 7]);
"#,
        "0 6\n1 9\n4\n7\n",
    );
    for (source, operation) in [
        (
            "readonly let a := [1]; for value in a.map((x:Int)=>{x=2;x}) { wt value; }",
            "__iter_next__",
        ),
        (
            "readonly let a := [[1]]; for value in a.map((readonly x:Vec<Int>)=>x) { let alias := value[0]; alias = 2; }",
            "__assign__",
        ),
    ] {
        for options in [&[][..], &["--no-opt"][..]] {
            let output = Case::new(source).compile_with_options(options);
            assert_eq!(output.status.code(), Some(1), "{source}");
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(
                error.contains("readonly") && error.contains(operation),
                "{error}"
            );
        }
    }
}

#[test]
fn readonly_vector_filter_preserves_cached_aliases_and_copied_positions() {
    runs(
        r#"
fn read(readonly noescape values: Vec<Int>) {
    let cursor := values.filter((readonly x: Int) => x % 2 == 1);
    let shared := cursor;
    wt __iter_has_next__(cursor), __iter_has_next__(shared);
    let detached = cursor;
    wt __iter_next__(cursor), __iter_next__(shared), __iter_next__(detached);
    for (i, value) in values.filter((readonly x: Int) => x > 1).map((readonly x: Int) => x * 10).enumerate() {
        wt i, value;
    }
}
read([1, 2, 3, 5]);
"#,
        "Yes Yes\n1 3 1\n0 20\n1 30\n2 50\n",
    );
    for (source, operation) in [
        (
            "readonly let a := [1]; for value in a.filter((x:Int)=>{x=2;true}) { wt value; }",
            "__iter_has_next__",
        ),
        (
            "readonly let a := [[1]]; let it := a.filter((readonly x:Vec<Int>)=>true); if __iter_has_next__(it) { let alias := __iter_next__(it)[0]; alias = 2; }",
            "__assign__",
        ),
        (
            "fn leak(noescape values:Vec<Int>)->Int{let it := values.filter((readonly x:Int)=>true); __iter_has_next__(it); return __iter_next__(it);} leak([1]);",
            "noescape",
        ),
    ] {
        for options in [&[][..], &["--no-opt"][..]] {
            let output = Case::new(source).compile_with_options(options);
            assert_eq!(output.status.code(), Some(1), "{source}");
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(
                error.contains(operation) && error.contains("main.rusk:"),
                "{error}"
            );
        }
    }
}

#[test]
fn iterator_adapters_keep_readonly_element_types() {
    runs(
        r#"
let values: Vec<readonly Int> := [4, 7];
for (i, value) in values.enumerate() { wt i, value; }
for (_, value) in zip(0 .. 2, values) { wt value; }
for value in values.map((readonly x: Int) => x + 1) { wt value; }
for value in values.filter((readonly x: Int) => x > 4) { wt value; }
"#,
        "0 4\n1 7\n4\n7\n5\n8\n7\n",
    );
    for adapter in ["map((readonly x:Int)=>x)", "filter((readonly x:Int)=>true)"] {
        let source =
            format!("let values:Vec<readonly Int>:=[1]; for x in values.{adapter} {{ x=2; }}");
        for options in [&[][..], &["--no-opt"][..]] {
            let output = Case::new(&source).compile_with_options(options);
            assert_eq!(output.status.code(), Some(1), "{source}");
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(
                error.contains("readonly") && error.contains("__assign__"),
                "{error}"
            );
        }
    }
}

#[test]
fn mismatches_and_structural_changes_keep_runtime_locations() {
    for (source, prefix, expected) in [
        (
            r#"let empty:Vec<Int>:=[];let other:=(0..2).map((x:Int)=>{wt "next";x});wt "before";zip(empty,other).to_vec();"#,
            "before\n",
            "zip inputs have different lengths",
        ),
        (
            r#"let empty:Vec<Int>:=[];let other:=(0..2).filter((x:Int)=>{wt x;true});wt "before";zip(empty,other).to_vec();"#,
            "before\n0\n",
            "zip inputs have different lengths",
        ),
        (
            r#"let cursor:=(1..3).map((x:Int)=>x);wt "before";zip(cursor,cursor);"#,
            "before\n",
            "zip requires distinct iterator states",
        ),
        (
            r#"for (i,x) in [1,2].enumerate(9223372036854775807){wt i,x;}"#,
            "9223372036854775807 1\n",
            "enumerate index overflow",
        ),
        (
            r#"let a:=[1,2];readonly let view:=a;let cursor:=view.filter((readonly x:Int)=>true);wt __iter_has_next__(cursor);a.push_back(3);__iter_next__(cursor);"#,
            "Yes\n",
            "container changed during iteration",
        ),
        (
            r#"let a:=[1,2];let cursor:=a.reversed();a.push_back(3);wt "before";cursor.to_vec();"#,
            "before\n",
            "container changed during iteration",
        ),
        (
            r#"let a:="ab";let cursor:=a.reversed();wt "before";a.push_back('c');wt cursor.to_vec();"#,
            "before\n",
            "string has active character references",
        ),
    ] {
        let output = Case::new(source).run("");
        assert_eq!(output.status.code(), Some(1), "{source}");
        assert_eq!(output.stdout, prefix.as_bytes());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(
            error.contains("main.rusk:1:") && error.contains(expected),
            "{error}"
        );
    }
}

#[test]
fn reverse_owner_contract_keeps_restrictions_and_lifetimes() {
    runs(
        r#"
fn total(readonly noescape r: Range) -> Int {
    let sum := 0;
    for x in r.reversed() { sum += x; }
    return sum;
}
wt total(1 .. 4);
let s := "ab";
let held: Char;
{
    let it := s.reversed();
    held := __iter_next__(it);
}
s := "cd";
held = 'X';
wt held, s;
"#,
        "6\nX cd\n",
    );
    for (source, diagnostic) in [
        (
            "fn leak(noescape a:Vec<Int>)->ReverseVecIter<Int>{return a.reversed();}leak([1]);",
            "noescape",
        ),
        (
            "readonly let a:=[1];for value in a.reversed(){value=2;}",
            "readonly",
        ),
    ] {
        let output = Case::new(source).compile();
        assert_eq!(output.status.code(), Some(1), "{source}");
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(
            error.contains("main.rusk:") && error.contains(diagnostic),
            "{error}"
        );
    }
}

#[test]
fn readonly_sequence_queries_and_reductions_compose_with_adapters() {
    runs(
        r#"
fn summarize(readonly noescape values: Vec<Int>) {
    wt values.all((readonly x: Int) => x > 0);
    wt values.any((readonly x: Int) => x == 2);
    wt values.fold(0, (sum: Int, readonly x: Int) => { sum += x; sum });
    wt values.scan(0, (sum: Int, readonly x: Int) => sum + x);
    let cursor := values.filter((readonly x: Int) => x > 1);
    wt cursor.any((readonly x: Int) => x == 2);
    for x in cursor { wt x; }
    wt values.map((readonly x: Int) => x + 1).fold(0, (sum: Int, readonly x: Int) => sum + x);
}
summarize([1, 2, 3]);
let empty: Vec<Int> := [];
summarize(empty);
"#,
        "Yes\nYes\n6\n0 1 3 6\nYes\n3\n9\nYes\nNo\n0\n0\nNo\n0\n",
    );
}

#[test]
fn readonly_reduction_checks_feedback_and_distinguishes_copied_snapshots() {
    runs(
        r#"
readonly let values := [2, 4];
let snapshots := values.scan(0, (sum: Int, readonly x: Int) => x);
snapshots[1] = 9;
wt snapshots, values;
let end := values.fold(0, (sum: Int, readonly x: Int) => x);
wt end;
"#,
        "0 9 4 2 4\n4\n",
    );
    for (source, diagnostic) in [
        (
            "readonly let a := [1]; a.all((x:Int)=>{x=2;true});",
            "argument 1 of 'all'",
        ),
        (
            "readonly let a := [1]; a.any((x:Int)=>{x=2;true});",
            "argument 1 of 'any'",
        ),
        (
            "readonly let a := [1,2]; a.fold(0,(sum:Int,readonly x:Int)=>{sum+=1;x});",
            "argument 1 of 'fold'",
        ),
        (
            "readonly let a := [1,2]; a.scan(0,(sum:Int,readonly x:Int)=>{sum+=1;x});",
            "argument 1 of 'scan'",
        ),
        (
            "readonly let a := [1]; let selected:=a.fold(0,(s:Int,readonly x:Int)=>x); selected=2;",
            "cannot modify readonly access",
        ),
        (
            r#"struct Row { value:Int; } fn __copy__(x:Row)->Row{return x;}
readonly let a := [Row{value:1}];
let snapshots := a.scan(Row{value:0}, (acc:Row,readonly x:Row)=>x);
snapshots[1].value=2;"#,
            "cannot modify readonly access",
        ),
        (
            "fn leak(noescape a:Vec<Int>)->Int{return a.fold(0,(s:Int,readonly x:Int)=>x);} wt leak([1]);",
            "noescape",
        ),
    ] {
        for options in [&[][..], &["--no-opt"][..]] {
            let output = Case::new(source).compile_with_options(options);
            assert_eq!(output.status.code(), Some(1), "{source}");
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(error.contains(diagnostic), "{error}");
        }
    }
}

#[test]
fn readonly_to_vec_shares_elements_without_restricting_fresh_results() {
    runs(
        r#"
let original := [[1], [2]];
readonly let values := original;
let collected := values.filter((readonly x: Vec<Int>) => x[0] == 2).to_vec();
original[1][0] = 7;
collected.push_back([3]);
wt collected[0], collected[1], original.len();
let independent := values.map((readonly x: Vec<Int>) => x[0] + 1).to_vec();
independent[0] = 9;
wt independent, original[0][0];
"#,
        "7 3 2\n9 8 1\n",
    );
    for (source, diagnostic) in [
        (
            "readonly let a := [[1]]; let b:=a.to_vec(); let alias:=b[0]; alias[0]=2;",
            "cannot modify readonly access",
        ),
        (
            "readonly let a := [1]; let b:=a.map((readonly x:Int)=>x).to_vec(); b[0]=2;",
            "cannot modify readonly access",
        ),
        (
            "let a:Vec<readonly Int>:=[1]; let b:=a.to_vec(); b[0]=2;",
            "cannot modify readonly access",
        ),
        (
            "fn leak(noescape a:Vec<Int>)->Vec<Int>{return a.to_vec();} leak([1]);",
            "noescape",
        ),
    ] {
        for options in [&[][..], &["--no-opt"][..]] {
            let output = Case::new(source).compile_with_options(options);
            assert_eq!(output.status.code(), Some(1), "{source}");
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(error.contains(diagnostic), "{error}");
        }
    }
}

#[test]
fn callbacks_can_infer_lambda_parameters() {
    let source = r#"
fn apply<T, F>(x: T, callback: F) -> T { return callback(x); }
struct Card { value: Int; }
let cards := [Card { value: 3 }, Card { value: 1 }];
cards.sort_by_key(card => card.value);
let calls := 0;
let values := cards.map(card => { calls += 1; card.value });
wt calls;
wt values.filter(x => x > 1).to_vec(), calls;
wt [1, 2, 3].fold(0, (acc, x) => acc + x);
wt apply(5, x => x + 2);
readonly let xs := [1, 2];
wt xs.map(x => x + 1).sum();
match true { ok if ok => { wt 8; } _ => {} }
"#;
    let expected = "0\n3 2\n6\n7\n5\n8\n";
    runs(source, expected);
    let typed = source
        .replace("card =>", "(card: Card) =>")
        .replace("x =>", "(x: Int) =>")
        .replace("(acc, x) =>", "(acc: Int, x: Int) =>");
    runs(&typed, expected);

    for (source, expected) in [
        (
            "let f := x => x;",
            "cannot infer type of lambda parameter 'x'",
        ),
        (
            "readonly let xs := [1]; wt xs.map(x => { x = 2; x }).sum();",
            "readonly",
        ),
    ] {
        let case = Case::new(source);
        for options in [&[][..], &["--no-opt"][..]] {
            let output = case.compile_with_options(options);
            assert!(!output.status.success());
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains(expected), "{error}");
        }
    }
}

#[test]
fn enumerate_accepts_default_named_and_legacy_starts() {
    runs(
        r#"
readonly let values := [3, 4];
wt enumerate(values).to_vec();
wt enumerate(values, start: 1).to_vec();
wt values.enumerate(start: 1).to_vec();
wt values.enumerate(1).to_vec();
let iter := enumerate;
wt iter(values).to_vec();
wt iter(values, start: 2).to_vec();
extern fn native_index<T>(source: Vec<T>) -> Enumerated<Iter<Vec<T>>, T> {
    code: "Obj<@{Enumerated<Iter<Vec<T>>,T>}> @{fn}(Obj<@{Vec<T>}> source, Loc loc) { return @{call enumerate(Vec<T>)->Enumerated<Iter<Vec<T>>,T>}(source, loc); }",
}
wt native_index([4, 5]).to_vec();
"#,
        "0 3 1 4\n1 3 2 4\n1 3 2 4\n1 3 2 4\n0 3 1 4\n2 3 3 4\n0 4 1 5\n",
    );
}
