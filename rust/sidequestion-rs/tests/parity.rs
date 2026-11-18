//! Parity harness for `sidequestion-rs`. Replays `cases.jsonl` (recorded from the
//! real Go `sidequestion.Parent.Key` by `record.sh`) against the Rust port.

use serde_json::{Value, json};

use sidequestion_rs::port::Parent;

fn cases() -> Vec<Value> {
    include_str!("../cases.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON case"))
        .collect()
}

fn arg_i64(args: &[Value], i: usize) -> i64 {
    args[i].as_i64().expect("int arg")
}

#[test]
fn parity_all_cases() {
    let all = cases();
    assert!(!all.is_empty(), "cases.jsonl must not be empty");
    let mut mismatches: Vec<String> = Vec::new();
    for c in &all {
        let name = c["fn"].as_str().expect("fn");
        assert_eq!(name, "Parent.Key", "unexpected fn {name}");
        let args: Vec<Value> = c["args"].as_array().expect("args array").clone();
        // args = [conversation_id, task_id, exploration_id, intent_id]
        let p = Parent {
            conversation_id: arg_i64(&args, 0),
            task_id: arg_i64(&args, 1),
            exploration_id: arg_i64(&args, 2),
            intent_id: arg_i64(&args, 3),
        };
        let actual = json!(p.key());
        let expected = &c["ok"];
        if &actual != expected {
            mismatches.push(format!(
                "{name} args={args:?}\n   expected={expected}\n   actual  ={actual}"
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} / {} cases mismatched:\n\n{}",
        mismatches.len(),
        all.len(),
        mismatches.join("\n")
    );
}
