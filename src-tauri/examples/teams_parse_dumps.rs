//! Parse saved Teams dumps and summarise what Wren reads (meetings plan Task 14).
//!   cargo run --example teams_parse_dumps -- <dump.json>...
fn main() {
    use std::collections::BTreeMap;
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for p in std::env::args().skip(1) {
        let d: wren_lib::meetings::teams::snapshot::Dump = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let o = wren_lib::meetings::teams::parse(&d.tree, &d.title);
        let key = format!("self={:?} muted={:?} roster={} speaking={:?} title={}", o.self_name.is_some(), o.self_muted, o.roster.len(), o.speaking.len(), o.title.is_some());
        *seen.entry(key).or_default() += 1;
    }
    for (k, n) in seen {
        println!("{n:>4}  {k}");
    }
}
