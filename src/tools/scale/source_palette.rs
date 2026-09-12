use crate::document::CancellationToken;
use crate::error::Result;
use image::RgbaImage;
use palette::{Lab, color_difference::Ciede2000};
use std::collections::{BTreeMap, HashMap};

const TRANSPARENT: u32 = u32::MAX;

pub(crate) fn delta_e(a: [f32; 3], b: [f32; 3]) -> f32 {
    let a: Lab = Lab::new(a[0], a[1], a[2]);
    a.difference(Lab::new(b[0], b[1], b[2]))
}

pub(crate) struct Palette {
    pub labels: Vec<u32>,
    pub representatives: Vec<usize>,
}

impl Palette {
    pub fn new(
        source: &RgbaImage,
        lab: &[[f32; 3]],
        occupancy: &[bool],
        threshold: f32,
        cancellation: &CancellationToken,
    ) -> Result<Self> {
        let mut histogram = BTreeMap::<[u8; 3], (usize, usize)>::new();
        for (q, pixel) in source.pixels().enumerate() {
            if q % source.width() as usize == 0 {
                cancellation.check()?;
            }
            if !occupancy[q] {
                continue;
            }
            let rgb = [pixel[0], pixel[1], pixel[2]];
            histogram.entry(rgb).or_insert((q, 0)).1 += 1;
        }
        let mut colours: Vec<_> = histogram.into_iter().collect();
        // Frequent source colours become fixed representatives. No centroid
        // updates, transitive unions, palette-size cap, or rare-colour pruning.
        colours.sort_unstable_by(|(a, (_, na)), (b, (_, nb))| nb.cmp(na).then(a.cmp(b)));
        let mut representatives = Vec::new();
        let mut members = Vec::<Vec<usize>>::new();
        let mut bins = HashMap::<(i16, i16, i16), Vec<usize>>::new();
        let mut assignments = HashMap::new();
        let bin = |p: [f32; 3]| {
            (
                (p[0] / 4.0).floor() as i16,
                (p[1] / 8.0).floor() as i16,
                (p[2] / 8.0).floor() as i16,
            )
        };
        for (rgb, (q, _)) in colours {
            cancellation.check()?;
            let (l, a, b) = bin(lab[q]);
            let mut best: Option<(usize, f32)> = None;
            if threshold > 0.0 {
                // The spatial shortlist may retain extra entries; only exact
                // CIEDE2000 checks authorize a merge. Complete-link membership
                // prevents a chain of near neighbours swallowing an isolate.
                for dl in -1..=1 {
                    for da in -1..=1 {
                        for db in -1..=1 {
                            if let Some(ids) = bins.get(&(l + dl, a + da, b + db)) {
                                for &id in ids {
                                    let d = delta_e(lab[q], lab[representatives[id]]);
                                    if d < threshold
                                        && best.is_none_or(|(old, cost)| {
                                            d < cost || (d == cost && id < old)
                                        })
                                        && members[id]
                                            .iter()
                                            .all(|p| delta_e(lab[q], lab[*p]) < threshold)
                                    {
                                        best = Some((id, d));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let id = best.map(|(id, _)| id).unwrap_or_else(|| {
                let id = representatives.len();
                representatives.push(q);
                members.push(Vec::new());
                bins.entry((l, a, b)).or_default().push(id);
                id
            });
            members[id].push(q);
            assignments.insert(rgb, id as u32);
        }
        let mut labels = vec![TRANSPARENT; occupancy.len()];
        for (q, pixel) in source.pixels().enumerate() {
            if q % source.width() as usize == 0 {
                cancellation.check()?;
            }
            if occupancy[q] {
                labels[q] = assignments[&[pixel[0], pixel[1], pixel[2]]];
            }
        }
        Ok(Self {
            labels,
            representatives,
        })
    }

    pub fn source(&self, sample: usize) -> usize {
        let label = self.labels[sample];
        if label == TRANSPARENT {
            sample
        } else {
            self.representatives[label as usize]
        }
    }
}
