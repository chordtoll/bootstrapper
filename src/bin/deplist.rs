use bootstrapper::helpers::gen_graph_all;
use petgraph::algo::toposort;

fn main() {
    let g = gen_graph_all();
    let order: Vec<_> = toposort(&g,None).unwrap().iter().map(|x| g.node_weight(*x).unwrap()).rev().collect();
    for i in order {
        println!("    - {}",String::from(i));
    }
}