//! Fixed evaluation procedures, Amendment 01. No code in this module is used
//! by resize(). Sobel boundaries and fixed directional ridge profiles are
//! independent measurements, not the production detector or ground truth.
use super::*;
mod fixtures;
use std::{
    collections::{BTreeSet, VecDeque},
    fmt::Write as _,
    path::Path,
};

#[derive(Clone, Debug)]
struct Feature {
    id: usize,
    kind: Kind,
    q: V,
    normal: Option<V>,
    strength: f64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Counts {
    tp: usize,
    fp: usize,
    fn_: usize,
}
impl Counts {
    fn csv(self) -> String {
        let metric = |n: usize, d: usize| {
            if d == 0 {
                "N/A".into()
            } else {
                format!("{:.6}", n as f64 / d as f64)
            }
        };
        format!(
            "{},{},{},{},{},{},{},{}",
            self.tp,
            self.fp,
            self.fn_,
            metric(self.tp, self.tp + self.fp),
            metric(self.tp, self.tp + self.fn_),
            metric(self.tp, self.tp + self.fp + self.fn_),
            metric(2 * self.tp, 2 * self.tp + self.fp + self.fn_),
            if self.tp + self.fp + self.fn_ == 0 {
                "empty-correct"
            } else {
                "nonempty"
            }
        )
    }
    fn add(&mut self, other: Self) {
        self.tp += other.tp;
        self.fp += other.fp;
        self.fn_ += other.fn_;
    }
}

fn exact(g: &[Feature], q: &[Feature]) -> Counts {
    let pixels = |features: &[Feature]| {
        features
            .iter()
            .map(|f| {
                (
                    f.kind,
                    (f.q[0] + 0.5).floor() as i64,
                    (f.q[1] + 0.5).floor() as i64,
                )
            })
            .collect::<BTreeSet<_>>()
    };
    let g = pixels(g);
    let q = pixels(q);
    let tp = g.intersection(&q).count();
    Counts {
        tp,
        fp: q.len() - tp,
        fn_: g.len() - tp,
    }
}

#[derive(Clone, Copy)]
struct ArcEdge {
    to: usize,
    reverse: usize,
    available: bool,
    cost: ExactCost,
}

// Exact sums of the already computed f64 squared distances. A common binary
// denominator 2^1074 represents every finite nonnegative f64 cost <= 1. The
// 18 limbs also cover sums over the bounded graph. Residual reverse arcs then
// cancel exactly: floating accumulation had created a spurious zero-cost
// predecessor cycle on quarter-phase vertical-line fixtures. No epsilon,
// quantized distance, or greedy fallback is used to conceal that failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExactCost {
    negative: bool,
    words: [u64; 18],
}
impl ExactCost {
    const ZERO: Self = Self {
        negative: false,
        words: [0; 18],
    };
    fn from_f64(value: f64) -> Self {
        assert!(value.is_finite() && (0.0..=1.0).contains(&value));
        let bits = value.to_bits();
        let exponent = ((bits >> 52) & 0x7ff) as usize;
        let mantissa = (bits & ((1_u64 << 52) - 1)) | if exponent == 0 { 0 } else { 1_u64 << 52 };
        let shift = exponent.saturating_sub(1);
        let word = shift / 64;
        let offset = shift % 64;
        let mut out = Self::ZERO;
        out.words[word] = mantissa << offset;
        if offset != 0 {
            out.words[word + 1] = mantissa >> (64 - offset);
        }
        out
    }
    fn negated(mut self) -> Self {
        if self.words.iter().any(|&w| w != 0) {
            self.negative = !self.negative;
        }
        self
    }
    fn magnitude_cmp(&self, other: &Self) -> Ordering {
        self.words.iter().rev().cmp(other.words.iter().rev())
    }
    fn add(self, other: Self) -> Self {
        if self.negative == other.negative {
            let mut out = Self {
                negative: self.negative,
                ..Self::ZERO
            };
            let mut carry = 0_u128;
            for (i, w) in out.words.iter_mut().enumerate() {
                let sum = u128::from(self.words[i]) + u128::from(other.words[i]) + carry;
                *w = sum as u64;
                carry = sum >> 64;
            }
            assert_eq!(carry, 0);
            out
        } else {
            let (a, b) = if self.magnitude_cmp(&other) == Ordering::Less {
                (other, self)
            } else {
                (self, other)
            };
            let mut out = Self {
                negative: a.negative,
                ..Self::ZERO
            };
            let mut borrow = false;
            for (i, w) in out.words.iter_mut().enumerate() {
                let (v, b1) = a.words[i].overflowing_sub(b.words[i]);
                let (v, b2) = v.overflowing_sub(u64::from(borrow));
                *w = v;
                borrow = b1 || b2;
            }
            assert!(!borrow);
            if out.words.iter().all(|&w| w == 0) {
                out.negative = false;
            }
            out
        }
    }
}
impl Ord for ExactCost {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.negative, other.negative) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => self.magnitude_cmp(other),
            (true, true) => other.magnitude_cmp(self),
        }
    }
}
impl PartialOrd for ExactCost {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
fn arc(graph: &mut [Vec<ArcEdge>], from: usize, to: usize, cost: f64) {
    let cost = ExactCost::from_f64(cost);
    let a = graph[from].len();
    let b = graph[to].len();
    graph[from].push(ArcEdge {
        to,
        reverse: b,
        available: true,
        cost,
    });
    graph[to].push(ArcEdge {
        to: from,
        reverse: a,
        available: false,
        cost: cost.negated(),
    });
}

/// Successive shortest augmenting paths on the residual graph: maximum
/// cardinality first, minimum total squared displacement second. Inputs and
/// graph adjacency are ordered by stable IDs. Equal-distance relaxations retain
/// the first stable predecessor; no epsilon costs perturb the distance goal.
fn matching(
    g: &[Feature],
    q: &[Feature],
    tolerance: f64,
    cancel: &CancellationToken,
) -> Result<Vec<(usize, usize, f64)>> {
    if g.len() + q.len() > 30_000 {
        return Err(Error::EvaluationLimit("sample", 30_000));
    }
    let mut gi: Vec<usize> = (0..g.len()).collect();
    gi.sort_by_key(|&i| g[i].id);
    let mut qi: Vec<usize> = (0..q.len()).collect();
    qi.sort_by_key(|&i| q[i].id);
    let sink = 1 + g.len() + q.len();
    let mut graph = vec![Vec::new(); sink + 1];
    let mut bins: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (j, &index) in qi.iter().enumerate() {
        bins.entry((q[index].q[0].floor() as i64, q[index].q[1].floor() as i64))
            .or_default()
            .push(j);
    }
    let mut edge_count = 0;
    for (i, &index) in gi.iter().enumerate() {
        check(cancel)?;
        arc(&mut graph, 0, 1 + i, 0.0);
        let a = &g[index];
        let mut neighbors = Vec::new();
        let radius = tolerance.ceil() as i64;
        let key = (a.q[0].floor() as i64, a.q[1].floor() as i64);
        for y in key.1 - radius..=key.1 + radius {
            for x in key.0 - radius..=key.0 + radius {
                if let Some(list) = bins.get(&(x, y)) {
                    for &j in list {
                        let b = &q[qi[j]];
                        let d = sub(a.q, b.q);
                        let d2 = dot(d, d);
                        if a.kind == b.kind
                            && d2 <= tolerance * tolerance
                            && !matches!((a.normal,b.normal),(Some(a),Some(b)) if dot(a,b).abs()<3.0_f64.sqrt()/2.0)
                        {
                            neighbors.push((j, d2));
                        }
                    }
                }
            }
        }
        neighbors.sort_by_key(|p| p.0);
        for (j, d2) in neighbors {
            edge_count += 1;
            if edge_count > 250_000 {
                return Err(Error::EvaluationLimit("edge", 250_000));
            }
            arc(&mut graph, 1 + i, 1 + g.len() + j, d2);
        }
    }
    for j in 0..q.len() {
        arc(&mut graph, 1 + g.len() + j, sink, 0.0);
    }
    loop {
        check(cancel)?;
        let mut distance = vec![None; graph.len()];
        let mut previous = vec![None; graph.len()];
        let mut queued = vec![false; graph.len()];
        let mut visits = vec![0; graph.len()];
        let mut queue = VecDeque::from([0]);
        distance[0] = Some(ExactCost::ZERO);
        queued[0] = true;
        let mut work = 0;
        while let Some(v) = queue.pop_front() {
            work += 1;
            if work % 1024 == 0 {
                check(cancel)?;
            }
            queued[v] = false;
            visits[v] += 1;
            if visits[v] > graph.len() {
                return Err(Error::EvaluationNumerical);
            }
            for (i, e) in graph[v].iter().enumerate() {
                if !e.available {
                    continue;
                }
                let next = distance[v].unwrap().add(e.cost);
                if distance[e.to].is_none_or(|old| next < old) {
                    distance[e.to] = Some(next);
                    previous[e.to] = Some((v, i));
                    if !queued[e.to] {
                        queue.push_back(e.to);
                        queued[e.to] = true;
                    }
                }
            }
        }
        if previous[sink].is_none() {
            break;
        }
        let mut v = sink;
        let mut path_length = 0;
        while v != 0 {
            path_length += 1;
            if path_length > graph.len() {
                return Err(Error::EvaluationNumerical);
            }
            if path_length % 1024 == 0 {
                check(cancel)?;
            }
            let (from, i) = previous[v].unwrap();
            let reverse = graph[from][i].reverse;
            graph[from][i].available = false;
            graph[v][reverse].available = true;
            v = from;
        }
    }
    let mut pairs = Vec::new();
    for i in 0..g.len() {
        for e in &graph[1 + i] {
            if !e.available && e.to > g.len() && e.to < sink {
                let j = qi[e.to - 1 - g.len()];
                let d = sub(g[gi[i]].q, q[j].q);
                pairs.push((gi[i], j, dot(d, d)));
            }
        }
    }
    Ok(pairs)
}

fn percentile(mut values: Vec<f64>, fraction: f64) -> String {
    if values.is_empty() {
        return "N/A".into();
    }
    values.sort_by(f64::total_cmp);
    format!(
        "{:.6}",
        values[((fraction * values.len() as f64).ceil() as usize)
            .saturating_sub(1)
            .min(values.len() - 1)]
    )
}

fn compare(
    g: &[Feature],
    q: &[Feature],
    prefix: &str,
    csv: &mut String,
    cancel: &CancellationToken,
) -> Result<Counts> {
    let exact_counts = exact(g, q);
    writeln!(
        csv,
        "{prefix},exact-rounded-pixels,{},N/A,N/A",
        exact_counts.csv()
    )
    .unwrap();
    for tolerance in [0.5, 1.0] {
        let pairs = matching(g, q, tolerance, cancel)?;
        let tp = pairs.len();
        let counts = Counts {
            tp,
            fp: q.len() - tp,
            fn_: g.len() - tp,
        };
        let distances: Vec<f64> = pairs.iter().map(|p| p.2.sqrt()).collect();
        writeln!(
            csv,
            "{prefix},matched-subpixel-{tolerance},{},{},{}",
            counts.csv(),
            percentile(distances.clone(), 0.5),
            percentile(distances, 0.95)
        )
        .unwrap();
    }
    Ok(exact_counts)
}

/// Frozen evaluator: no Gaussian bank, Hessian, production NMS, hysteresis,
/// alpha eligibility, adaptive scores, or source deduplication. Boundaries use
/// per-channel Sobel / 8; ridge centers use signed 3-point profiles at radii
/// 1 and 2 along four fixed normals. Both use an absolute 0.03 response floor
/// and parabolic localization of a local response maximum.
fn measured(image: &RgbaImage, cancel: &CancellationToken) -> Result<Vec<Feature>> {
    let w = image.width() as usize;
    let h = image.height() as usize;
    let p = base::decode(image, cancel)?;
    let mut normals = vec![[[0.0; 2]; 2]; w * h];
    let mut responses = vec![[0.0; 2]; w * h];
    for y in 2..h.saturating_sub(2) {
        check(cancel)?;
        for x in 2..w.saturating_sub(2) {
            let q = [x as f64, y as f64];
            let center = p[y * w + x];
            for (c, _) in center.iter().enumerate() {
                let mut g = [0.0; 2];
                for (d, weight) in [(-1, 1.0), (0, 2.0), (1, 1.0)] {
                    g[0] += (p[(y as isize + d) as usize * w + x + 1][c]
                        - p[(y as isize + d) as usize * w + x - 1][c])
                        * weight
                        / 8.0;
                    g[1] += (p[(y + 1) * w + (x as isize + d) as usize][c]
                        - p[(y - 1) * w + (x as isize + d) as usize][c])
                        * weight
                        / 8.0;
                }
                let magnitude = g[0].hypot(g[1]);
                if magnitude > responses[y * w + x][1] {
                    responses[y * w + x][1] = magnitude;
                    normals[y * w + x][1] = [g[0] / magnitude, g[1] / magnitude];
                }
            }
            for normal in [[1.0, 0.0], [COS_45, COS_45], [0.0, 1.0], [-COS_45, COS_45]] {
                for radius in [1.0, 2.0] {
                    let minus = sample(&p, w, h, offset(q, normal, -radius));
                    let plus = sample(&p, w, h, offset(q, normal, radius));
                    for c in 0..4 {
                        let dm = center[c] - minus[c];
                        let dp = center[c] - plus[c];
                        if dm * dp > 0.0 {
                            let response = dm.abs().min(dp.abs());
                            if response > responses[y * w + x][0] {
                                responses[y * w + x][0] = response;
                                normals[y * w + x][0] = normal;
                            }
                        }
                    }
                }
            }
        }
    }
    let mut features = Vec::new();
    for y in 2..h.saturating_sub(2) {
        check(cancel)?;
        for x in 2..w.saturating_sub(2) {
            for k in 0..2 {
                let response = responses[y * w + x][k];
                if response < 0.03 {
                    continue;
                }
                let normal = normals[y * w + x][k];
                let q = [x as f64, y as f64];
                let minus = sample(&responses, w, h, offset(q, normal, -1.0))[k];
                let plus = sample(&responses, w, h, offset(q, normal, 1.0))[k];
                if response >= minus && response > plus {
                    let denominator = minus - 2.0 * response + plus;
                    let delta = if denominator < -1e-12 {
                        (0.5 * (minus - plus) / denominator).clamp(-0.5, 0.5)
                    } else {
                        0.0
                    };
                    features.push(Feature {
                        id: (y * w + x) * 2 + k,
                        kind: if k == 0 { Kind::Ridge } else { Kind::Edge },
                        q: offset(q, normal, delta),
                        normal: Some(normal),
                        strength: response,
                    });
                }
            }
        }
    }
    Ok(features)
}

fn from_entries(
    reference: &Reference,
    entries: &[Option<Entry>],
    width: usize,
    source_positions: bool,
) -> Vec<Feature> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for e in entries.iter().flatten() {
        if source_positions && !seen.insert(e.candidate) {
            continue;
        }
        let c = &reference.candidates[e.candidate];
        out.push(Feature {
            id: if source_positions { c.id } else { e.pixel },
            kind: c.kind,
            q: if source_positions {
                e.position
            } else {
                [(e.pixel % width) as f64, (e.pixel / width) as f64]
            },
            normal: Some(e.normal),
            strength: e.score,
        });
    }
    out
}

fn write_features(dir: &Path, name: &str, features: &[Feature], w: u32, h: u32) {
    let mut csv = String::from("id,kind,x,y,nx,ny,strength\n");
    let mut image = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 255]));
    for f in features {
        let n = f.normal.unwrap_or([f64::NAN; 2]);
        writeln!(
            csv,
            "{},{:?},{},{},{},{},{}",
            f.id, f.kind, f.q[0], f.q[1], n[0], n[1], f.strength
        )
        .unwrap();
        let x = (f.q[0] + 0.5).floor() as i64;
        let y = (f.q[1] + 0.5).floor() as i64;
        if x >= 0 && y >= 0 && x < i64::from(w) && y < i64::from(h) {
            image.put_pixel(
                x as u32,
                y as u32,
                if f.kind == Kind::Ridge {
                    Rgba([255, 100, 40, 255])
                } else {
                    Rgba([40, 160, 255, 255])
                },
            );
        }
    }
    std::fs::write(dir.join(format!("{name}.csv")), csv).unwrap();
    image.save(dir.join(format!("{name}.png"))).unwrap();
}

pub(super) fn diagnostics(
    reference: &Reference,
    out: &Output,
    dir: &Path,
    prefix: &str,
    cancel: &CancellationToken,
) -> Result<()> {
    let start = Instant::now();
    let (w, h) = out.image.dimensions();
    let projected = selection::projected_evidence(reference, &out.base, cancel)?;
    let s = from_entries(reference, &projected, w as usize, true);
    let b = measured(&out.base, cancel)?;
    let o = measured(&out.image, cancel)?;
    let m = from_entries(reference, &out.diagnostics.owners, w as usize, false);
    for (name, f) in [("S", &s), ("B", &b), ("M", &m), ("O", &o)] {
        write_features(dir, &format!("{prefix}-{name}"), f, w, h);
    }
    let mut csv = String::from(
        "comparison,kind,representation,tp,fp,fn,precision,recall,iou,f1,status,median_displacement,p95_displacement\n",
    );
    let mut changes = String::from(
        "kind,source_absent_in_B_present_in_O,output_unsupported_by_source_detector\n",
    );
    for kind in [Kind::Ridge, Kind::Edge] {
        let of_kind = |features: &[Feature]| {
            features
                .iter()
                .filter(|f| f.kind == kind)
                .cloned()
                .collect::<Vec<_>>()
        };
        let ss = of_kind(&s);
        let bb = of_kind(&b);
        let oo = of_kind(&o);
        compare(&ss, &bb, &format!("S-v-B,{kind:?}"), &mut csv, cancel)?;
        compare(&ss, &oo, &format!("S-v-O,{kind:?}"), &mut csv, cancel)?;
        let sb = matching(&ss, &bb, 1.0, cancel)?;
        let so = matching(&ss, &oo, 1.0, cancel)?;
        let base_ids: BTreeSet<usize> = sb.iter().map(|p| p.0).collect();
        let recovered = so.iter().filter(|p| !base_ids.contains(&p.0)).count();
        writeln!(changes, "{kind:?},{recovered},{}", oo.len() - so.len()).unwrap();
    }
    std::fs::write(dir.join(format!("{prefix}-disagreement.csv")), csv).unwrap();
    std::fs::write(dir.join(format!("{prefix}-changes.csv")), changes).unwrap();
    std::fs::write(dir.join(format!("{prefix}-evaluation-time.txt")),format!("{:?}; includes extraction, matching and diagnostic file I/O; excluded from resize timing\n",start.elapsed())).unwrap();
    Ok(())
}

fn f(id: usize, x: f64, y: f64) -> Feature {
    Feature {
        id,
        kind: Kind::Ridge,
        q: [x, y],
        normal: Some([0.0, 1.0]),
        strength: 1.0,
    }
}

#[test]
fn matching_does_not_reward_doubled_lanes_and_uses_augmenting_paths() {
    let cancel = CancellationToken::default();
    let g = vec![f(0, 0.0, 0.0), f(1, 1.0, 0.0)];
    let q = vec![
        f(0, 0.5, 0.0),
        f(1, -0.5, 0.0),
        f(2, 0.0, 1.0),
        f(3, 1.0, 1.0),
    ];
    let pairs = matching(&g, &q, 0.5, &cancel).unwrap();
    assert_eq!(pairs.len(), 2);
    assert!(pairs.contains(&(0, 1, 0.25)));
    assert!(pairs.contains(&(1, 0, 0.25)));
    let doubled = matching(&g, &q, 1.0, &cancel).unwrap();
    assert_eq!(doubled.len(), 2);
    assert_eq!(q.len() - doubled.len(), 2);
    assert!(
        Counts::default()
            .csv()
            .contains("N/A,N/A,N/A,N/A,empty-correct")
    );
    assert_eq!(exact(&[], &q).tp, 0);
    assert_eq!(exact(&[], &q).fp, 4);
    let rotated = vec![Feature {
        normal: Some([1.0, 0.0]),
        ..f(9, 0.0, 0.0)
    }];
    assert!(matching(&g, &rotated, 1.0, &cancel).unwrap().is_empty());
}

#[test]
fn assignment_minimum_cost_and_stable_id_ties() {
    let cancel = CancellationToken::default();
    let g = vec![f(0, 0.0, 0.0), f(1, 0.8, 0.0)];
    let q = vec![f(0, 0.1, 0.0), f(1, 0.7, 0.0)];
    let pairs = matching(&g, &q, 1.0, &cancel).unwrap();
    assert_eq!(pairs.len(), 2);
    assert!(pairs.iter().map(|p| p.2).sum::<f64>() < 0.021);
    let g = vec![f(10, 0.0, 0.0), f(20, 0.0, 0.0)];
    let q = vec![f(30, 0.0, 0.0), f(40, 0.0, 0.0)];
    let pairs = matching(&g, &q, 0.5, &cancel).unwrap();
    let mut reversed = g.clone();
    reversed.reverse();
    let r = matching(&reversed, &q, 0.5, &cancel).unwrap();
    let ids = |a: &[Feature], pairs: Vec<(usize, usize, f64)>| {
        pairs
            .iter()
            .map(|p| (a[p.0].id, q[p.1].id))
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(ids(&g, pairs), ids(&reversed, r));
}

#[test]
fn exact_residual_costs_cancel_without_epsilon_or_cycles() {
    for x in [0.0, 1.0, 0.1, 0.3, 1e-300, f64::from_bits(1)] {
        let a = ExactCost::from_f64(x);
        assert_eq!(a.add(a.negated()), ExactCost::ZERO);
        for y in [1.0, 0.2, 1e-200] {
            let b = ExactCost::from_f64(y);
            assert_eq!(a.add(b).add(a.negated()), b);
            assert_eq!(a.add(b).add(b.negated()), a);
        }
    }
    // Exhaustively enumerate all assignments for small non-binary geometries,
    // independently of residual-graph traversal and its ordering.
    fn visit(
        g: &[Feature],
        q: &[Feature],
        i: usize,
        used: u32,
        count: usize,
        cost: ExactCost,
        best: &mut (usize, ExactCost),
    ) {
        if i == g.len() {
            if count > best.0 || (count == best.0 && cost < best.1) {
                *best = (count, cost);
            }
            return;
        }
        visit(g, q, i + 1, used, count, cost, best);
        for j in 0..q.len() {
            let d = sub(g[i].q, q[j].q);
            let d2 = dot(d, d);
            if used & (1 << j) == 0 && d2 <= 1.0 {
                visit(
                    g,
                    q,
                    i + 1,
                    used | (1 << j),
                    count + 1,
                    cost.add(ExactCost::from_f64(d2)),
                    best,
                );
            }
        }
    }
    for phase in [0.1, 0.25, 0.3, 0.7] {
        let g: Vec<_> = (0..4).map(|i| f(i, i as f64 * 0.7, phase)).collect();
        let q: Vec<_> = (0..4).map(|i| f(i, i as f64 * 0.6 + phase, 0.3)).collect();
        let mut best = (0, ExactCost::ZERO);
        visit(&g, &q, 0, 0, 0, ExactCost::ZERO, &mut best);
        let pairs = matching(&g, &q, 1.0, &CancellationToken::default()).unwrap();
        let cost = pairs.iter().fold(ExactCost::ZERO, |cost, p| {
            cost.add(ExactCost::from_f64(p.2))
        });
        assert_eq!((pairs.len(), cost), best);
    }
}
