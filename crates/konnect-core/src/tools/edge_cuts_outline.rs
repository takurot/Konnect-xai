//! Which Edge.Cuts segments form a board's outer outline, and which form
//! cutouts inside it (#130).
//!
//! Segments are grouped into loops by shared endpoints. The loop with the
//! largest bounding box is the outer outline. A closed loop whose bounding box
//! lies strictly inside the outer one is a cutout. Everything else (open
//! strays, leftover outlines that overlap the outer one) belongs to the
//! outline being replaced. Bounding-box containment cannot tell a cutout from
//! a nested leftover outline, nor see the notch of an L-shaped board; both are
//! treated as cutouts.

use std::collections::HashMap;

/// A point in KiCad's internal unit, nanometres.
pub(crate) type NmPoint = (i64, i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NmBox {
    pub min: NmPoint,
    pub max: NmPoint,
}

impl NmBox {
    fn around(points: impl IntoIterator<Item = NmPoint>) -> Option<Self> {
        points.into_iter().fold(None, |acc, (x, y)| {
            Some(match acc {
                None => NmBox {
                    min: (x, y),
                    max: (x, y),
                },
                Some(b) => NmBox {
                    min: (b.min.0.min(x), b.min.1.min(y)),
                    max: (b.max.0.max(x), b.max.1.max(y)),
                },
            })
        })
    }

    fn area(&self) -> i128 {
        i128::from(self.max.0 - self.min.0) * i128::from(self.max.1 - self.min.1)
    }

    pub fn strictly_inside(&self, outer: &NmBox) -> bool {
        self.min.0 > outer.min.0
            && self.min.1 > outer.min.1
            && self.max.0 < outer.max.0
            && self.max.1 < outer.max.1
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OutlineSplit {
    /// Indices of the segments that make up the outline being replaced.
    pub replaced: Vec<usize>,
    /// Bounding box of every cutout loop that stays.
    pub cutouts: Vec<NmBox>,
}

pub(crate) fn mm_to_nm(mm: f64) -> i64 {
    (mm * 1e6).round() as i64
}

pub(crate) fn split_outline(segments: &[(NmPoint, NmPoint)]) -> OutlineSplit {
    let loops = connected_loops(segments);
    let boxes: Vec<NmBox> = loops
        .iter()
        .map(|members| {
            NmBox::around(members.iter().flat_map(|&i| [segments[i].0, segments[i].1]))
                .expect("a loop has at least one segment")
        })
        .collect();
    let Some(outer) = boxes
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.area().cmp(&b.1.area()).then(b.0.cmp(&a.0)))
        .map(|(i, b)| (i, *b))
    else {
        return OutlineSplit {
            replaced: Vec::new(),
            cutouts: Vec::new(),
        };
    };

    let is_cutout = |i: usize| {
        i != outer.0 && boxes[i].strictly_inside(&outer.1) && is_closed(segments, &loops[i])
    };
    let mut replaced: Vec<usize> = (0..loops.len())
        .filter(|&i| !is_cutout(i))
        .flat_map(|i| loops[i].iter().copied())
        .collect();
    replaced.sort_unstable();
    OutlineSplit {
        replaced,
        cutouts: (0..loops.len())
            .filter(|&i| is_cutout(i))
            .map(|i| boxes[i])
            .collect(),
    }
}

/// Segment indices grouped by shared endpoints, in first-seen order.
fn connected_loops(segments: &[(NmPoint, NmPoint)]) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..segments.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut owner: HashMap<NmPoint, usize> = HashMap::new();
    for (i, &(a, b)) in segments.iter().enumerate() {
        for p in [a, b] {
            match owner.get(&p) {
                Some(&j) => {
                    let (ri, rj) = (root(&mut parent, i), root(&mut parent, j));
                    parent[ri] = rj;
                }
                None => {
                    owner.insert(p, i);
                }
            }
        }
    }
    let mut order: Vec<usize> = Vec::new();
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..segments.len() {
        let r = root(&mut parent, i);
        groups
            .entry(r)
            .or_insert_with(|| {
                order.push(r);
                Vec::new()
            })
            .push(i);
    }
    order
        .into_iter()
        .map(|r| groups.remove(&r).unwrap_or_default())
        .collect()
}

/// A loop is closed when every vertex is shared by an even number of
/// segment ends.
fn is_closed(segments: &[(NmPoint, NmPoint)], members: &[usize]) -> bool {
    let mut degree: HashMap<NmPoint, usize> = HashMap::new();
    for &i in members {
        *degree.entry(segments[i].0).or_default() += 1;
        *degree.entry(segments[i].1).or_default() += 1;
    }
    degree.values().all(|d| d % 2 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x1: i64, y1: i64, x2: i64, y2: i64) -> Vec<(NmPoint, NmPoint)> {
        vec![
            ((x1, y1), (x2, y1)),
            ((x2, y1), (x2, y2)),
            ((x2, y2), (x1, y2)),
            ((x1, y2), (x1, y1)),
        ]
    }

    #[test]
    fn keeps_closed_inner_loops_and_replaces_outer_and_strays() {
        let segments = [
            rect(0, 0, 50, 40),
            rect(10, 10, 20, 15),
            vec![((30, 30), (35, 30))],
        ]
        .concat();
        let split = split_outline(&segments);
        assert_eq!(split.replaced, vec![0, 1, 2, 3, 8]);
        assert_eq!(
            split.cutouts,
            vec![NmBox {
                min: (10, 10),
                max: (20, 15)
            }]
        );
    }

    /// #314's overlapping leftover outlines still go: neither is inside the other.
    #[test]
    fn replaces_overlapping_leftover_outlines() {
        let segments = [rect(0, 0, 20, 10), rect(5, 5, 45, 30)].concat();
        let split = split_outline(&segments);
        assert_eq!(split.replaced, (0..8).collect::<Vec<_>>());
        assert!(split.cutouts.is_empty());
    }

    /// Known limit: a leftover outline nested fully inside reads as a cutout.
    #[test]
    fn a_nested_leftover_outline_reads_as_a_cutout() {
        let segments = [rect(0, 0, 40, 40), rect(10, 10, 30, 30)].concat();
        assert_eq!(split_outline(&segments).replaced, vec![0, 1, 2, 3]);
    }

    #[test]
    fn a_rectangle_sharing_an_edge_with_the_outer_one_is_not_a_cutout() {
        let segments = [rect(0, 0, 40, 40), rect(0, 0, 20, 20)].concat();
        assert_eq!(
            split_outline(&segments).replaced,
            (0..8).collect::<Vec<_>>()
        );
    }

    #[test]
    fn nothing_to_split_on_an_empty_board() {
        let split = split_outline(&[]);
        assert!(split.replaced.is_empty() && split.cutouts.is_empty());
    }

    #[test]
    fn mm_rounds_to_whole_nanometres() {
        assert_eq!(mm_to_nm(120.0000004), 120_000_000);
        assert_eq!(mm_to_nm(-0.5), -500_000);
    }
}
