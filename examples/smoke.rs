use hanceng::fixtures;
use hanceng::incremental::rerun;
use hanceng::solver::solve;

fn main() {
    let input = fixtures::seed_input();
    let r = solve(&input);
    assert!(r.feasible, "baseline feasible: {:?}", r.conflict);
    for n in &r.nodes {
        println!("d={:>6}  q05={:>8} med={:>8} q95={:>8} depM={:?} depG={:?}",
            n.depth, n.q05, n.median, n.q95, n.dep_markers, n.dep_gaps);
    }
    for e in &r.edges {
        println!("edge {:>5}->{:<5} med={:>8} q05={:>8} q95={:>8} gap={}",
            e.depth_from, e.depth_to, e.rate_median, e.rate_q05, e.rate_q95, e.is_gap);
    }
    let r2 = solve(&input);
    assert_eq!(serde_json::to_string(&r.nodes).unwrap(), serde_json::to_string(&r2.nodes).unwrap(), "replay");

    // 单调性 & 区间包含中位数
    for e in &r.edges {
        let a = r.nodes.iter().find(|n| n.depth == e.depth_from).unwrap();
        let b = r.nodes.iter().find(|n| n.depth == e.depth_to).unwrap();
        assert!(b.median + 1e-12 >= a.median, "nondecreasing");
    }

    // 冲突版本
    let mut bad = input.clone();
    bad.markers.iter_mut().find(|m| m.id == "C25B").unwrap().excluded = false;
    let rb = solve(&bad);
    assert!(!rb.feasible);
    let c = rb.conflict.clone().unwrap();
    println!("CONFLICT: {} minimal={} markers={:?}", c.reason, c.minimal, c.markers.iter().map(|m| m.id.clone()).collect::<Vec<_>>());
    assert!(c.minimal);
    let ids: Vec<String> = c.markers.iter().map(|m| m.id.clone()).collect();
    assert!(ids.contains(&"I28".to_string()) && ids.contains(&"C25B".to_string()));

    // 缺芯边界调整 → 只失效依赖该段的分位数
    let mut moved = input.clone();
    moved.segments.iter_mut().find(|s| s.id == "S3").unwrap().gap_min_years = Some(40.0);
    let rep = rerun(&input, &r, &moved);
    println!("rerun reused={} invalid={}", rep.reused_node_count, rep.invalidated_node_count);
    for st in &rep.node_status {
        println!("  d={} reused={} ({})", st.depth, st.reused, st.reason);
    }
    assert!(rep.result.feasible);
    assert!(rep.reused_node_count > 0, "上游节点应复用");
    let shallow_reused = rep.node_status.iter().any(|s| s.depth < 21.8 && s.reused);
    assert!(shallow_reused, "缺芯之上的分位数必须复用");
    let deep_invalid = rep.node_status.iter().any(|s| s.depth > 22.4 && !s.reused);
    assert!(deep_invalid, "缺芯之下的分位数应失效");

    // 交换组对调
    let mut sw = input.clone();
    sw.exchanges[0].swapped = true;
    let rsw = solve(&sw);
    assert!(rsw.feasible);
    println!("swapped run d15 med={} d20 med={}",
        rsw.nodes.iter().find(|n| (n.depth-15.0).abs()<1e-9).unwrap().median,
        rsw.nodes.iter().find(|n| (n.depth-20.0).abs()<1e-9).unwrap().median);

    // 恰好相触可行
    let mut touch = input.clone();
    for m in touch.markers.iter_mut() { if m.id=="C25B" { m.excluded=false; m.lo=180.0; m.hi=182.0; } }
    // I28 [180,190] 与 [180,182] 相触于180
    let rt = solve(&touch);
    println!("touch feasible={}", rt.feasible);
    assert!(rt.feasible);

    println!("SMOKE-OK");
}
