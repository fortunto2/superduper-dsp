//! Loading: what the parser must refuse, not only what it accepts.

use connectome_core::{Graph, GraphError};
use std::io::Cursor;

#[test]
fn a_small_edge_list_round_trips() {
    let csv = "pre_id,post_id,syn_count\n1,2,10\n1,3,4\n2,3,7\n";
    let g = Graph::from_edge_list(Cursor::new(csv), 5).unwrap();
    // The 1→3 edge has 4 synapses and is below the threshold, so two edges survive over
    // three neurons — and neuron 3 is still a node, because 2→3 kept it.
    assert_eq!(g.edges(), 2);
    assert_eq!(g.neurons(), 3);
    assert_eq!(g.row_start.len(), g.neurons() + 1);
    assert_eq!(*g.row_start.last().unwrap() as usize, g.edges());
}

#[test]
fn a_malformed_row_names_its_line_number() {
    let csv = "pre_id,post_id,syn_count\n1,2,10\nnot,a,row\n";
    match Graph::from_edge_list(Cursor::new(csv), 1) {
        Err(GraphError::MalformedRow { line, .. }) => assert_eq!(line, 3),
        other => panic!("expected a malformed row naming line 3, got {other:?}"),
    }
}

#[test]
fn an_empty_result_is_an_error_rather_than_an_empty_graph() {
    // Threshold above every edge. Returning an empty graph here would integrate happily
    // and report zero spikes, which reads exactly like a quiet brain.
    let csv = "pre_id,post_id,syn_count\n1,2,3\n";
    assert!(matches!(Graph::from_edge_list(Cursor::new(csv), 99), Err(GraphError::Empty)));
}

#[test]
fn synthetic_graphs_are_deterministic_and_labelled() {
    let a = Graph::synthetic(50, 4, 9);
    let b = Graph::synthetic(50, 4, 9);
    assert_eq!(a.targets, b.targets);
    assert_eq!(a.weights, b.weights);
    assert_eq!(a.edges(), 200);
}
