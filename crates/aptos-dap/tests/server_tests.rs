mod helpers;

use aptos_dap::server::RunCommand;
use expect_test::expect;
use helpers::{build_test_package, DapTestServer, RECV_TIMEOUT};
use std::time::Duration;

#[test]
fn test_per_frame_locals() {
    // language=Move
    let pkg = build_test_package(
        r#"
module 0x42::test {
    fun inner(a: u64, _val: signer): u64 {
        a // bp1
    }

    #[test(acc = @0x1)]
    fun test_it(acc: signer) {
        let _result = inner(42, acc);
    }
}
"#,
    );
    let mode = RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);
    t.initialize_and_launch_test(&pkg);

    t.assert_frame_scopes(0, expect![[r#"
        [
          {
            "name": "Locals",
            "presentationHint": "locals",
            "variablesReference": 1000,
            "expensive": false
          }
        ]"#]]);
    t.assert_frame_scopes(1, expect![[r#"
        [
          {
            "name": "Locals",
            "presentationHint": "locals",
            "variablesReference": 1001,
            "expensive": false
          }
        ]"#]]);

    t.assert_frame_variables(0, expect![[r#"
        [
          {
            "name": "a",
            "value": "42",
            "variablesReference": 0
          },
          {
            "name": "_val",
            "value": "signer(0x1)",
            "variablesReference": 0
          }
        ]"#]]);
    t.assert_frame_variables(1, expect![[r#"
        [
          {
            "name": "acc",
            "value": "signer(0x1)",
            "variablesReference": 0
          }
        ]"#]]);
}

#[test]
fn test_source_line_breakpoint() {
    // language=Move
    let pkg = build_test_package(
        r#"
module 0x42::test {
    fun add(a: u64, b: u64): u64 { a + b }

    #[test]
    fun test_bp() {
        let a = add(1, 1);
        assert!(a == 2); // bp1
    }
}
"#,
    );
    let mode = RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);
    t.initialize_and_launch_test(&pkg);

    t.assert_stack_frames(expect![[r#"
        [
          {
            "id": 0,
            "name": "0x42::test::test_bp",
            "source": {
              "name": "test.move",
              "path": "$TMPDIR/sources/test.move"
            },
            "line": 8,
            "column": 0
          }
        ]"#]]);
}

#[test]
fn test_stack_frame_names() {
    // language=Move
    let pkg = build_test_package(
        r#"
module 0x42::test {
    fun inner(val: u64): u64 {
        assert!(val > 0); // bp1
        val
    }

    #[test]
    fun test_it() {
        let result = inner(42);
        assert!(result == 42);
    }
}
"#,
    );
    let mode = RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);
    t.initialize_and_launch_test(&pkg);

    t.assert_stack_frames(expect![[r#"
        [
          {
            "id": 0,
            "name": "0x42::test::inner",
            "source": {
              "name": "test.move",
              "path": "$TMPDIR/sources/test.move"
            },
            "line": 4,
            "column": 0
          },
          {
            "id": 1,
            "name": "0x42::test::test_it",
            "source": {
              "name": "test.move",
              "path": "$TMPDIR/sources/test.move"
            },
            "line": 10,
            "column": 0
          }
        ]"#]]);
}

#[test]
fn test_snapshot_no_stale() {
    // language=Move
    let pkg = build_test_package(
        r#"
module 0x42::test {
    fun consume_u64(_val: u64) {}

    fun first_helper(x: u64) {
        consume_u64(x);
    }

    fun second_helper(y: u64): u64 {
        consume_u64(y); // bp1
        y
    }

    #[test]
    fun test_it() {
        first_helper(111);
        let _ = second_helper(222);
    }
}
"#,
    );
    let mode = RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);
    t.initialize_and_launch_test(&pkg);

    t.assert_frame_variables(0, expect![[r#"
        [
          {
            "name": "y",
            "value": "222",
            "variablesReference": 0
          }
        ]"#]]);
}

#[test]
fn test_value_types() {
    // language=Move
    let pkg = build_test_package(
        r#"
module 0x42::test {
    fun consume(
        _a: bool, _b: u8, _c: u64, _d: u128, _e: address, _f: vector<u64>
    ) {}

    fun helper(
        my_bool: bool, my_u8: u8, my_u64: u64,
        my_u128: u128, my_address: address, my_vec: vector<u64>
    ): u64 {
        consume(my_bool, my_u8, my_u64, my_u128, my_address, my_vec); // bp1
        my_u64
    }

    #[test]
    fun test_types() {
        let _ = helper(true, 42, 1000, 99999, @0xCAFE, vector[1u64, 2, 3]);
    }
}
"#,
    );
    let mode = RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);
    t.initialize_and_launch_test(&pkg);

    t.assert_frame_variables(0, expect![[r#"
        [
          {
            "name": "my_bool",
            "value": "true",
            "variablesReference": 0
          },
          {
            "name": "my_u8",
            "value": "42",
            "variablesReference": 0
          },
          {
            "name": "my_u64",
            "value": "1000",
            "variablesReference": 0
          },
          {
            "name": "my_u128",
            "value": "99999",
            "variablesReference": 0
          },
          {
            "name": "my_address",
            "value": "000000000000000000000000000000000000000000000000000000000000cafe",
            "variablesReference": 0
          },
          {
            "name": "my_vec",
            "value": "[1, 2, 3]",
            "variablesReference": 100000
          }
        ]"#]]);
}

#[test]
#[ignore] // nested struct field names not yet resolved
fn test_nested_struct_fields() {
    // language=Move
    let pkg = build_test_package(
        r#"
module 0x42::test {
    struct Inner has drop { x: u64, y: bool }
    struct Outer has drop { inner: Inner, tag: u64 }

    fun consume(_o: Outer) {}

    fun helper(o: Outer): u64 {
        consume(o); // bp1
        42
    }

    #[test]
    fun test_it() {
        let o = Outer { inner: Inner { x: 100, y: true }, tag: 7 };
        let _ = helper(o);
    }
}
"#,
    );
    let mode = RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);
    t.initialize_and_launch_test(&pkg);

    t.assert_frame_variables(0, expect![[r#""#]]);
}

#[test]
fn test_no_duplicate_copy_vars() {
    // language=Move
    let pkg = build_test_package(
        r#"
module 0x42::test {
    struct CopyStruct has copy, drop { val: u64 }

    fun consume_copy_struct(_s: CopyStruct) {}

    fun use_three_times(s: CopyStruct): u64 {
        consume_copy_struct(s);
        consume_copy_struct(s);
        consume_copy_struct(s);
        s.val // bp1
    }

    #[test]
    fun test_no_dup() {
        let s = CopyStruct { val: 42 };
        let _ = use_three_times(s);
    }
}
"#,
    );
    let mode = RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);
    t.initialize_and_launch_test(&pkg);

    t.assert_frame_variables(0, expect![[r#"
        [
          {
            "name": "s",
            "value": "{ val: 42 }",
            "variablesReference": 100000
          }
        ]"#]]);
}

#[test]
fn test_source_bp_no_duplicate_hit() {
    // language=Move
    let pkg = build_test_package(
        r#"
module 0x42::test {
    fun multi_bytecode_line(a: u64, b: u64): u64 {
        let result = a + b; // bp1
        result
    }

    #[test]
    fun test_bp() {
        let x = multi_bytecode_line(10, 20);
        assert!(x == 30);
    }
}
"#,
    );
    let mode = RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);
    t.initialize_and_launch_test(&pkg);

    t.assert_stack_frames(expect![[r#"
        [
          {
            "id": 0,
            "name": "0x42::test::multi_bytecode_line",
            "source": {
              "name": "test.move",
              "path": "$TMPDIR/sources/test.move"
            },
            "line": 4,
            "column": 0
          },
          {
            "id": 1,
            "name": "0x42::test::test_bp",
            "source": {
              "name": "test.move",
              "path": "$TMPDIR/sources/test.move"
            },
            "line": 10,
            "column": 0
          }
        ]"#]]);

    t.send("continue", Some(serde_json::json!({ "threadId": 1 })));
    t.collect_until_event("terminated", 30);
}

#[test]
fn test_source_bp_no_duplicate_hit_with_function_call() {
    // language=Move
    let pkg = build_test_package(
        r#"
module 0x42::test {
    fun is_valid(): bool { true }

    fun guarded_action(): u64 {
        assert!(is_valid(), 42); // bp1
        100
    }

    #[test]
    fun test_bp() {
        let x = guarded_action();
        assert!(x == 100);
    }
}
"#,
    );
    let mode = RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);
    t.initialize_and_launch_test(&pkg);

    t.assert_stack_frames(expect![[r#"
        [
          {
            "id": 0,
            "name": "0x42::test::guarded_action",
            "source": {
              "name": "test.move",
              "path": "$TMPDIR/sources/test.move"
            },
            "line": 6,
            "column": 0
          },
          {
            "id": 1,
            "name": "0x42::test::test_bp",
            "source": {
              "name": "test.move",
              "path": "$TMPDIR/sources/test.move"
            },
            "line": 12,
            "column": 0
          }
        ]"#]]);

    t.send("continue", Some(serde_json::json!({ "threadId": 1 })));
    t.collect_until_event("terminated", 30);
}

#[test]
fn test_source_bp_rehits_in_loop_statement() {
    // language=Move
    let pkg = build_test_package(
        r#"
module 0x42::test {
    fun loop_body(i: u64): u64 {
        i
    }

    #[test]
    fun test_loop() {
        let i = 0;
        while (i < 3) {
            i = loop_body(i); // bp1
        };
    }
}
"#,
    );
    let mode = RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);
    t.initialize_and_launch_test(&pkg);

    t.assert_stack_frames(expect![[r#"
        [
          {
            "id": 0,
            "name": "0x42::test::test_loop",
            "source": {
              "name": "test.move",
              "path": "$TMPDIR/sources/test.move"
            },
            "line": 11,
            "column": 0
          }
        ]"#]]);

    t.continue_until_breakpoint();
    t.assert_stack_frames(expect![[r#"
        [
          {
            "id": 0,
            "name": "0x42::test::test_loop",
            "source": {
              "name": "test.move",
              "path": "$TMPDIR/sources/test.move"
            },
            "line": 11,
            "column": 0
          }
        ]"#]]);

    t.continue_until_breakpoint();
    t.assert_stack_frames(expect![[r#"
        [
          {
            "id": 0,
            "name": "0x42::test::test_loop",
            "source": {
              "name": "test.move",
              "path": "$TMPDIR/sources/test.move"
            },
            "line": 11,
            "column": 0
          }
        ]"#]]);
}

#[test]
fn test_source_bp_rehits_in_loop_inner_function() {
    // language=Move
    let pkg = build_test_package(
        r#"
module 0x42::test {
    fun loop_body(i: u64): u64 {
        let next = i + 1; // bp1
        next
    }

    #[test]
    fun test_loop() {
        let i = 0;
        while (i < 3) {
            i = loop_body(i);
        };
    }
}
"#,
    );
    let mode = RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);
    t.initialize_and_launch_test(&pkg);

    t.assert_stack_frames(expect![[r#"
        [
          {
            "id": 0,
            "name": "0x42::test::loop_body",
            "source": {
              "name": "test.move",
              "path": "$TMPDIR/sources/test.move"
            },
            "line": 4,
            "column": 0
          },
          {
            "id": 1,
            "name": "0x42::test::test_loop",
            "source": {
              "name": "test.move",
              "path": "$TMPDIR/sources/test.move"
            },
            "line": 12,
            "column": 0
          }
        ]"#]]);

    t.continue_until_breakpoint();
    t.assert_stack_frames(expect![[r#"
        [
          {
            "id": 0,
            "name": "0x42::test::loop_body",
            "source": {
              "name": "test.move",
              "path": "$TMPDIR/sources/test.move"
            },
            "line": 4,
            "column": 0
          },
          {
            "id": 1,
            "name": "0x42::test::test_loop",
            "source": {
              "name": "test.move",
              "path": "$TMPDIR/sources/test.move"
            },
            "line": 12,
            "column": 0
          }
        ]"#]]);

    t.continue_until_breakpoint();
    t.assert_stack_frames(expect![[r#"
        [
          {
            "id": 0,
            "name": "0x42::test::loop_body",
            "source": {
              "name": "test.move",
              "path": "$TMPDIR/sources/test.move"
            },
            "line": 4,
            "column": 0
          },
          {
            "id": 1,
            "name": "0x42::test::test_loop",
            "source": {
              "name": "test.move",
              "path": "$TMPDIR/sources/test.move"
            },
            "line": 12,
            "column": 0
          }
        ]"#]]);
}

#[test]
fn test_step_over_line() {
    // language=Move
    let pkg = build_test_package(
        r#"
module 0x42::test {
    fun helper(a: u64, b: u64): (u64, u64, u64, u64, u64) {
        let sum = a + b; // bp1
        let doubled = sum * 2;
        let result = doubled + 1;
        (a, b, sum, doubled, result)
    }

    #[test]
    fun test_it() {
        let (_, _, _, _, _) = helper(10, 20);
    }
}
"#,
    );
    let mode = RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);
    t.initialize_and_launch_test(&pkg);

    t.assert_frame_variables(0, expect![[r#"
        [
          {
            "name": "a",
            "value": "10",
            "variablesReference": 0
          },
          {
            "name": "b",
            "value": "20",
            "variablesReference": 0
          }
        ]"#]]);

    t.step_over();
    t.assert_frame_variables(0, expect![[r#"
        [
          {
            "name": "a",
            "value": "10",
            "variablesReference": 0
          },
          {
            "name": "b",
            "value": "20",
            "variablesReference": 0
          },
          {
            "name": "$t8",
            "value": "30",
            "variablesReference": 0
          }
        ]"#]]);

    t.step_over();
    t.assert_frame_variables(0, expect![[r#"
        [
          {
            "name": "a",
            "value": "10",
            "variablesReference": 0
          },
          {
            "name": "b",
            "value": "20",
            "variablesReference": 0
          },
          {
            "name": "$t8",
            "value": "30",
            "variablesReference": 0
          },
          {
            "name": "$t10",
            "value": "60",
            "variablesReference": 0
          }
        ]"#]]);

    t.step_over();
    t.assert_frame_variables(0, expect![[r#"
        [
          {
            "name": "a",
            "value": "10",
            "variablesReference": 0
          },
          {
            "name": "b",
            "value": "20",
            "variablesReference": 0
          },
          {
            "name": "$t8",
            "value": "30",
            "variablesReference": 0
          },
          {
            "name": "$t10",
            "value": "60",
            "variablesReference": 0
          },
          {
            "name": "$t11",
            "value": "61",
            "variablesReference": 0
          }
        ]"#]]);
}

#[test]
fn test_signer_display() {
    // language=Move
    let pkg = build_test_package(
        r#"
module 0x42::test {
    fun use_signer(_s: &signer): u64 {
        42 // bp1
    }

    #[test(acc = @0x1)]
    fun test_it(acc: signer) {
        let _ = use_signer(&acc);
    }
}
"#,
    );
    let mode = RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);
    t.initialize_and_launch_test(&pkg);

    t.assert_frame_variables(0, expect![[r#"
        [
          {
            "name": "_s",
            "value": "(&) signer(0x1)",
            "variablesReference": 100000
          }
        ]"#]]);
}

#[test]
fn test_signer_display_after_move() {
    // language=Move
    let pkg = build_test_package(
        r#"
module 0x42::test {
    fun consume_signer(_s: signer) {}

    fun helper(s: signer): u64 {
        consume_signer(s);
        42 // bp1
    }

    #[test(acc = @0x1)]
    fun test_it(acc: signer) {
        let _ = helper(acc);
    }
}
"#,
    );
    let mode = RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);
    t.initialize_and_launch_test(&pkg);

    t.assert_frame_variables(0, expect![[r#"
        [
          {
            "name": "s",
            "value": "signer(0x1)",
            "variablesReference": 0
          }
        ]"#]]);
}

#[test]
fn test_string_variable() {
    // language=Move
    let pkg = build_test_package(
        r#"
module 0x1::string {
    struct String has copy, drop, store { bytes: vector<u8> }
    public fun utf8(bytes: vector<u8>): String { String { bytes } }
}

module 0x42::test {
    use 0x1::string;

    fun consume(_s: string::String) {}

    fun helper(s: string::String): u64 {
        consume(s); // bp1
        42
    }

    #[test]
    fun test_it() {
        let s = string::utf8(b"hello world");
        let _ = helper(s);
    }
}
"#,
    );
    let mode = RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);
    t.initialize_and_launch_test(&pkg);

    t.assert_frame_variables(0, expect![[r#"
        [
          {
            "name": "s",
            "value": "\"hello world\"",
            "variablesReference": 0
          }
        ]"#]]);
}

#[test]
fn test_warns_unreachable_breakpoint() {
    // language=Move
    let pkg = build_test_package(
        r#"
module 0x42::test {
    #[test]
    fun test_it() {
        let _ = 1 + 2; // bp1
    }
}
"#,
    );
    let mode = RunCommand::Test {
        filter: String::new(),
        package_path: pkg.path.clone(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);

    t.initialize();
    t.launch();

    let mut bps = pkg.all_bps();
    bps.push("/nonexistent/fake_module.move:10");
    t.set_breakpoints(&bps);

    t.send("configurationDone", None);
    let mut outputs = Vec::new();
    for _ in 0..30 {
        let m = t.collect_until_event_any(&["output", "stopped", "terminated"], RECV_TIMEOUT);
        if m["event"] == "output" {
            if let Some(text) = m["body"]["output"].as_str() {
                outputs.push(text.to_string());
            }
        }
        if m["event"] == "stopped" || m["event"] == "terminated" {
            break;
        }
    }

    let warning = outputs.iter().find(|o| o.contains("unreachable"));
    assert!(
        warning.is_some(),
        "expected a warning about unresolvable breakpoint, got outputs: {outputs:?}",
    );
    let warning = warning.unwrap();
    assert!(
        warning.contains("/nonexistent/fake_module.move"),
        "warning should mention the unresolvable file, got: {warning}",
    );
}

#[test]
#[ignore] // requires network access to mainnet
fn test_replay_basic() {
    let mode = RunCommand::Replay {
        txn_id: 4969730041,
        network: "mainnet".to_string(),
        local_packages: vec![],
        prebuilt_packages: vec![],
        named_addresses: std::collections::BTreeMap::new(),
        skip_fetch_latest_git_deps: true,
    };
    let mut t = DapTestServer::start(mode);
    t.initialize_and_launch_replay(&[], Duration::from_secs(120));

    let long_timeout = Duration::from_secs(120);
    let mut stop_count = 0;
    loop {
        let frames = t.get_stack_frames();
        assert!(!frames.is_empty(), "expected at least one stack frame");
        stop_count += 1;

        if !t.continue_execution_timeout(long_timeout) {
            break;
        }
    }
    assert!(stop_count >= 1, "should have stopped at least once");
}
