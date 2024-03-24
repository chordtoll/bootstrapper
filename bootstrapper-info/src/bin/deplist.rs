use bootstrapper_common::helpers::gen_graph_all;
use petgraph::algo::toposort;

fn main() {
    let g = gen_graph_all();
    let order: Vec<_> = toposort(&g, None)
        .unwrap_or_else(|x| panic!("Cycle detected: {}:{}",g[x.node_id()].name,g[x.node_id()].version))
        .iter()
        .map(|x| g.node_weight(*x).unwrap())
        .rev()
        .collect();
    for i in order {
        println!("    - {}", String::from(i));
    }
}
