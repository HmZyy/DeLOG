//! Coordinate assignment.

use super::order::ProperGraph;

/// Cross-axis centre coordinates for every vertex via the priority method,
/// aligning socket anchor points rather than node centres.
///
/// Layers start packed at minimum separation, then alternating sweeps move
/// each vertex toward the median of its fixed-side anchor targets, pushing
/// strictly lower-priority vertices along and never violating separation.
pub(super) fn assign_cross(g: &ProperGraph, size_cross: &[f32], node_gap: f32) -> Vec<f32> {
    /// Placement settles quickly; further sweeps barely move vertices.
    const HALF_SWEEPS: usize = 4;
    let mut x = vec![0.0f32; size_cross.len()];
    for layer in &g.layers {
        let mut cursor = 0.0;
        for (i, &v) in layer.iter().enumerate() {
            if i > 0 {
                cursor += separation(g.num_real, size_cross, node_gap, layer[i - 1], v);
            }
            x[v] = cursor;
        }
    }
    for sweep in 0..HALF_SWEEPS {
        if sweep % 2 == 0 {
            for l in 1..g.layers.len() {
                place_layer(g, &mut x, size_cross, node_gap, l, true);
            }
        } else {
            for l in (0..g.layers.len().saturating_sub(1)).rev() {
                place_layer(g, &mut x, size_cross, node_gap, l, false);
            }
        }
    }
    x
}

/// Main-axis centre coordinates: each layer occupies a band sized to its
/// largest vertex, with `layer_gap` between bands.
pub(super) fn assign_main(g: &ProperGraph, size_main: &[f32], layer_gap: f32) -> Vec<f32> {
    let mut main = vec![0.0f32; size_main.len()];
    let mut cursor = 0.0;
    for layer in &g.layers {
        let band = layer.iter().fold(0.0f32, |max, &v| max.max(size_main[v]));
        for &v in layer {
            main[v] = cursor + band * 0.5;
        }
        cursor += band + layer_gap;
    }
    main
}

/// The minimum distance between the centres of cross-axis neighbours.
///
/// Dummy vertices (edge channels) pack at half the node gap.
fn separation(num_real: usize, size_cross: &[f32], node_gap: f32, a: usize, b: usize) -> f32 {
    let gap = if a >= num_real || b >= num_real {
        node_gap * 0.5
    } else {
        node_gap
    };
    size_cross[a] * 0.5 + gap + size_cross[b] * 0.5
}

/// One placement pass over layer `l`, deriving desired positions from the
/// previous (`toward_back`) or next layer.
fn place_layer(
    g: &ProperGraph,
    x: &mut [f32],
    size_cross: &[f32],
    node_gap: f32,
    l: usize,
    toward_back: bool,
) {
    let layer = &g.layers[l];
    // Dummies have the highest priority (straightening long edges), then
    // vertices by fixed-side degree.
    let prio: Vec<usize> = layer
        .iter()
        .map(|&v| {
            if v >= g.num_real {
                usize::MAX
            } else if toward_back {
                g.back[v].len()
            } else {
                g.fwd[v].len()
            }
        })
        .collect();
    let mut order: Vec<usize> = (0..layer.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(prio[i]));
    for &i in &order {
        let v = layer[i];
        let segs = if toward_back { &g.back[v] } else { &g.fwd[v] };
        if segs.is_empty() {
            continue;
        }
        // Desired position: the median anchor-to-anchor alignment target.
        let mut targets: Vec<f32> = segs
            .iter()
            .map(|&s| {
                let seg = &g.segments[s];
                if toward_back {
                    x[seg.src] + seg.src_anchor - seg.dst_anchor
                } else {
                    x[seg.dst] + seg.dst_anchor - seg.src_anchor
                }
            })
            .collect();
        targets.sort_by(f32::total_cmp);
        let mid = targets.len() / 2;
        let desired = if targets.len() % 2 == 1 {
            targets[mid]
        } else {
            (targets[mid - 1] + targets[mid]) * 0.5
        };
        nudge(
            x, size_cross, node_gap, g.num_real, layer, &prio, i, desired,
        );
    }
}

/// Move `layer[i]` as close to `desired` as allowed, pushing strictly
/// lower-priority vertices along while maintaining minimum separation, and
/// never moving equal-or-higher-priority vertices.
#[allow(clippy::too_many_arguments)]
fn nudge(
    x: &mut [f32],
    size_cross: &[f32],
    node_gap: f32,
    num_real: usize,
    layer: &[usize],
    prio: &[usize],
    i: usize,
    desired: f32,
) {
    let v = layer[i];
    let sep = |a: usize, b: usize| separation(num_real, size_cross, node_gap, a, b);
    if desired > x[v] {
        let mut limit = f32::INFINITY;
        let mut sum_sep = 0.0;
        for j in i + 1..layer.len() {
            sum_sep += sep(layer[j - 1], layer[j]);
            if prio[j] >= prio[i] {
                limit = x[layer[j]] - sum_sep;
                break;
            }
        }
        x[v] = desired.min(limit).max(x[v]);
        for j in i + 1..layer.len() {
            let min_x = x[layer[j - 1]] + sep(layer[j - 1], layer[j]);
            if x[layer[j]] >= min_x {
                break;
            }
            x[layer[j]] = min_x;
        }
    } else if desired < x[v] {
        let mut limit = f32::NEG_INFINITY;
        let mut sum_sep = 0.0;
        for j in (0..i).rev() {
            sum_sep += sep(layer[j], layer[j + 1]);
            if prio[j] >= prio[i] {
                limit = x[layer[j]] + sum_sep;
                break;
            }
        }
        x[v] = desired.max(limit).min(x[v]);
        for j in (0..i).rev() {
            let max_x = x[layer[j + 1]] - sep(layer[j], layer[j + 1]);
            if x[layer[j]] <= max_x {
                break;
            }
            x[layer[j]] = max_x;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{CEdge, CGraph};
    use super::*;

    fn proper(cg: &CGraph) -> ProperGraph {
        let pairs: Vec<_> = cg.edges.iter().map(|e| (e.src, e.dst)).collect();
        let reversed = vec![false; pairs.len()];
        let layer = super::super::rank::assign_layers(cg.size_main.len(), &pairs);
        let mut g = super::super::order::build_proper(cg, &reversed, &layer);
        super::super::order::minimize_crossings(&mut g);
        g
    }

    fn padded_sizes(g: &ProperGraph, sizes: &[f32]) -> Vec<f32> {
        let mut all = vec![0.0; g.pos.len()];
        all[..sizes.len()].copy_from_slice(sizes);
        all
    }

    #[test]
    fn chain_with_offset_sockets_aligns_anchors() {
        // Each node's output sits 10 below centre and input 10 above, so
        // node centres must stagger by 20 for the edges to run straight.
        let cg = CGraph {
            size_main: vec![10.0; 3],
            size_cross: vec![30.0; 3],
            in_anchors: vec![vec![-10.0]; 3],
            out_anchors: vec![vec![10.0]; 3],
            edges: vec![
                CEdge {
                    src: 0,
                    src_socket: 0,
                    dst: 1,
                    dst_socket: 0,
                },
                CEdge {
                    src: 1,
                    src_socket: 0,
                    dst: 2,
                    dst_socket: 0,
                },
            ],
        };
        let g = proper(&cg);
        let size_cross = padded_sizes(&g, &cg.size_cross);
        let x = assign_cross(&g, &size_cross, 25.0);
        assert!((x[1] - x[0] - 20.0).abs() < 1e-3);
        assert!((x[2] - x[1] - 20.0).abs() < 1e-3);
    }

    #[test]
    fn layers_respect_minimum_separation() {
        // A bipartite graph forcing several nodes into each layer.
        let edges = [(0, 3), (0, 4), (1, 3), (1, 5), (2, 4), (2, 5)]
            .iter()
            .map(|&(src, dst)| CEdge {
                src,
                src_socket: 0,
                dst,
                dst_socket: 0,
            })
            .collect();
        let cg = CGraph {
            size_main: vec![10.0; 6],
            size_cross: vec![40.0; 6],
            in_anchors: vec![Vec::new(); 6],
            out_anchors: vec![Vec::new(); 6],
            edges,
        };
        let g = proper(&cg);
        let size_cross = padded_sizes(&g, &cg.size_cross);
        let node_gap = 25.0;
        let x = assign_cross(&g, &size_cross, node_gap);
        for layer in &g.layers {
            for pair in layer.windows(2) {
                let min_sep = separation(g.num_real, &size_cross, node_gap, pair[0], pair[1]);
                assert!(x[pair[1]] - x[pair[0]] >= min_sep - 1e-3);
            }
        }
        assert!(x.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn main_axis_bands_separated_by_layer_gap() {
        let cg = CGraph {
            size_main: vec![10.0, 30.0, 20.0],
            size_cross: vec![10.0; 3],
            in_anchors: vec![Vec::new(); 3],
            out_anchors: vec![Vec::new(); 3],
            edges: vec![
                CEdge {
                    src: 0,
                    src_socket: 0,
                    dst: 1,
                    dst_socket: 0,
                },
                CEdge {
                    src: 1,
                    src_socket: 0,
                    dst: 2,
                    dst_socket: 0,
                },
            ],
        };
        let g = proper(&cg);
        let size_main = padded_sizes(&g, &cg.size_main);
        let main = assign_main(&g, &size_main, 50.0);
        assert_eq!(main[0], 5.0);
        assert_eq!(main[1], 10.0 + 50.0 + 15.0);
        assert_eq!(main[2], 10.0 + 50.0 + 30.0 + 50.0 + 10.0);
    }
}
