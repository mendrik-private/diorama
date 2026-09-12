use super::*;

#[derive(Clone, Copy)]
pub(super) struct Geometry {
    pub w: usize,
    pub h: usize,
    pub sx: f64,
    pub sy: f64,
}
impl Geometry {
    pub fn transform(self, q: V) -> V {
        [(q[0] + 0.5) * self.sx - 0.5, (q[1] + 0.5) * self.sy - 0.5]
    }
    pub fn inverse(self, p: V) -> V {
        [(p[0] + 0.5) / self.sx - 0.5, (p[1] + 0.5) / self.sy - 0.5]
    }
    pub fn normal(self, n: V) -> V {
        norm([n[0] / self.sx, n[1] / self.sy]).expect("validated nonzero normal and positive scale")
    }
    fn point(self, i: usize) -> V {
        [(i % self.w) as f64, (i / self.w) as f64]
    }
    fn neighbors(self, i: usize) -> impl Iterator<Item = usize> {
        let x = (i % self.w) as isize;
        let y = (i / self.w) as isize;
        (-1..=1).flat_map(move |dy| {
            (-1..=1).filter_map(move |dx| {
                let xx = x + dx;
                let yy = y + dy;
                (xx >= 0
                    && yy >= 0
                    && xx < self.w as isize
                    && yy < self.h as isize
                    && (dx != 0 || dy != 0))
                    .then(|| yy as usize * self.w + xx as usize)
            })
        })
    }
}

fn rank(a: &Entry, b: &Entry, candidates: &[Candidate]) -> Ordering {
    b.score
        .total_cmp(&a.score)
        .then(
            candidates[b.candidate]
                .effective_z
                .total_cmp(&candidates[a.candidate].effective_z),
        )
        .then(a.distance_squared.total_cmp(&b.distance_squared))
        .then(candidates[a.candidate].id.cmp(&candidates[b.candidate].id))
        .then(a.pixel.cmp(&b.pixel))
}
fn normal_bin(n: V) -> usize {
    let normals = [[1.0, 0.0], [COS_45, COS_45], [0.0, 1.0], [-COS_45, COS_45]];
    let mut best = 0;
    let mut strength = dot(n, normals[0]).abs();
    for (i, &normal) in normals.iter().enumerate().skip(1) {
        let value = dot(n, normal).abs();
        if value > strength {
            strength = value;
            best = i;
        }
    }
    best
}

pub(super) fn transport(
    reference: &Reference,
    base: &RgbaImage,
    g: Geometry,
    diagnostics: &mut Diagnostics,
    cancel: &CancellationToken,
) -> Result<Vec<Option<Entry>>> {
    transport_impl(reference, base, g, diagnostics, cancel, true)
}

pub(super) fn projected_evidence(
    reference: &Reference,
    base: &RgbaImage,
    cancel: &CancellationToken,
) -> Result<Vec<Option<Entry>>> {
    let g = Geometry {
        w: base.width() as usize,
        h: base.height() as usize,
        sx: f64::from(base.width()) / f64::from(reference.source.width()),
        sy: f64::from(base.height()) / f64::from(reference.source.height()),
    };
    let mut diagnostics = Diagnostics {
        collisions: vec![0; g.w * g.h],
        ..Default::default()
    };
    transport_impl(reference, base, g, &mut diagnostics, cancel, false)
}

fn transport_impl(
    reference: &Reference,
    base: &RgbaImage,
    g: Geometry,
    diagnostics: &mut Diagnostics,
    cancel: &CancellationToken,
    require_eligibility: bool,
) -> Result<Vec<Option<Entry>>> {
    let mut slots = vec![None; g.w * g.h * 8];
    for (iteration, &i) in reference.active.iter().enumerate() {
        if iteration % 1024 == 0 {
            check(cancel)?;
        }
        let c = &reference.candidates[i];
        let u = g.transform(c.q);
        let n = g.normal(c.n);
        let x = u[0].floor() as isize;
        let y = u[1].floor() as isize;
        let mut splats = Vec::with_capacity(4);
        let mut maximum: f64 = 0.0;
        for yy in y..=y + 1 {
            for xx in x..=x + 1 {
                if xx < 0 || yy < 0 || xx >= g.w as isize || yy >= g.h as isize {
                    continue;
                }
                let point = [xx as f64, yy as f64];
                let a = (1.0 - (point[0] - u[0]).abs()).max(0.0)
                    * (1.0 - (point[1] - u[1]).abs()).max(0.0);
                if a > 0.0 {
                    maximum = maximum.max(a);
                    splats.push((yy as usize * g.w + xx as usize, point, a));
                }
            }
        }
        for (pixel, point, a) in splats {
            let color = if c.kind == Kind::Ridge {
                c.colors[0]
            } else if dot(sub(g.inverse(point), c.q), c.n) > 0.0 {
                c.colors[2]
            } else {
                c.colors[1]
            };
            if require_eligibility
                && ([color, c.colors[1], c.colors[2]]
                    .iter()
                    .any(|&i| reference.source.as_raw()[i * 4 + 3] != 255)
                    || base.as_raw()[pixel * 4 + 3] != 255)
            {
                diagnostics.ineligible_splats += 1;
                continue;
            }
            let d = sub(point, u);
            let entry = Entry {
                candidate: i,
                pixel,
                normal: n,
                position: u,
                score: c.effective_z * a / maximum,
                distance_squared: dot(d, d),
                color,
            };
            let slot = pixel * 8 + if c.kind == Kind::Ridge { 0 } else { 4 } + normal_bin(n);
            if let Some(previous) = slots[slot] {
                diagnostics.displaced += 1;
                diagnostics.collisions[pixel] = diagnostics.collisions[pixel].saturating_add(1);
                if rank(&entry, &previous, &reference.candidates) == Ordering::Less {
                    slots[slot] = Some(entry);
                }
            } else {
                slots[slot] = Some(entry);
            }
        }
    }
    Ok(slots)
}

pub(super) fn conflict(a: &Entry, b: &Entry, g: Geometry) -> bool {
    if a.pixel == b.pixel {
        return true;
    }
    let displacement = sub(g.point(a.pixel), g.point(b.pixel));
    if displacement[0].abs() > 1.0 || displacement[1].abs() > 1.0 {
        return false;
    }
    let e = norm(displacement).expect("distinct neighboring centers");
    dot(a.normal, b.normal).abs() >= COS_22
        && dot(e, a.normal).abs().max(dot(e, b.normal).abs()) >= COS_45
}
fn suppression(
    mut entries: Vec<Entry>,
    candidates: &[Candidate],
    g: Geometry,
    cancel: &CancellationToken,
) -> Result<Vec<Option<Entry>>> {
    sort(&mut entries, |a, b| rank(a, b, candidates), cancel)?;
    let mut accepted = vec![None; g.w * g.h];
    for (i, entry) in entries.into_iter().enumerate() {
        if i % 1024 == 0 {
            check(cancel)?;
        }
        if entry.score < 0.5 || accepted[entry.pixel].is_some() {
            continue;
        }
        if g.neighbors(entry.pixel).any(|p| {
            accepted[p]
                .as_ref()
                .is_some_and(|other| conflict(&entry, other, g))
        }) {
            continue;
        }
        accepted[entry.pixel] = Some(entry);
    }
    Ok(accepted)
}

pub(super) fn compatible(a: &Entry, b: &Entry, candidates: &[Candidate], g: Geometry) -> bool {
    let displacement = sub(g.point(a.pixel), g.point(b.pixel));
    if a.pixel == b.pixel || displacement[0].abs() > 1.0 || displacement[1].abs() > 1.0 {
        return false;
    }
    let e = norm(displacement).expect("distinct neighbors");
    let ca = &candidates[a.candidate];
    let cb = &candidates[b.candidate];
    let d = sub(ca.q, cb.q);
    let limit = 2.0 * (1.0 / g.sx).max(1.0 / g.sy) + 1.0;
    // Compatibility uses the original full-P evidence for the selected source
    // color: ridge center, edge selected side. Never averaged slot colors.
    let color = |entry: &Entry, c: &Candidate| {
        if c.kind == Kind::Ridge {
            c.center
        } else if dot(sub(g.inverse(g.point(entry.pixel)), c.q), c.n) > 0.0 {
            c.plus
        } else {
            c.minus
        }
    };
    distance(color(a, ca), color(b, cb)) <= 0.15
        && dot(d, d) <= limit * limit
        && dot(e, tangent(a.normal)).abs() >= 0.5
        && dot(e, tangent(b.normal)).abs() >= 0.5
}

pub(super) fn hysteresis(
    entries: &[Option<Entry>],
    candidates: &[Candidate],
    g: Geometry,
    cancel: &CancellationToken,
) -> Result<(Vec<Option<Entry>>, Vec<usize>)> {
    let mut labels = vec![0; entries.len()];
    let mut out = vec![None; entries.len()];
    let mut label = 0;
    for start in 0..entries.len() {
        if start % 1024 == 0 {
            check(cancel)?;
        }
        if entries[start].is_none() || labels[start] != 0 {
            continue;
        }
        label += 1;
        labels[start] = label;
        let mut component = vec![start];
        let mut cursor = 0;
        let mut strong = false;
        while cursor < component.len() {
            if cursor % 1024 == 0 {
                check(cancel)?;
            }
            let i = component[cursor];
            cursor += 1;
            let a = entries[i].as_ref().unwrap();
            strong |= a.score >= 1.0;
            for j in g.neighbors(i) {
                if labels[j] == 0
                    && entries[j]
                        .as_ref()
                        .is_some_and(|b| compatible(a, b, candidates, g))
                {
                    labels[j] = label;
                    component.push(j);
                }
            }
        }
        if strong {
            for i in component {
                out[i] = entries[i];
            }
        }
    }
    Ok((out, labels))
}

fn remove_flanks(
    edges: Vec<Entry>,
    ridges: &[Option<Entry>],
    candidates: &[Candidate],
    diagnostics: &mut Diagnostics,
    cancel: &CancellationToken,
) -> Result<Vec<Entry>> {
    // One index entry per surviving ridge. Choose bins at least as wide as
    // the largest association radius; the query then visits nine bins only.
    let mut bin: f64 = 8.0;
    for (i, entry) in ridges.iter().flatten().enumerate() {
        if i % 1024 == 0 {
            check(cancel)?;
        }
        let sigma = candidates[entry.candidate].sigma;
        bin = bin.max((2.0 * sigma).hypot(sigma.max(1.0)));
    }
    let mut bins: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (iteration, entry) in ridges.iter().flatten().enumerate() {
        if iteration % 1024 == 0 {
            check(cancel)?;
        }
        let c = &candidates[entry.candidate];
        bins.entry(((c.q[0] / bin).floor() as i64, (c.q[1] / bin).floor() as i64))
            .or_default()
            .push(entry.candidate);
    }
    let mut out = Vec::new();
    for (iteration, entry) in edges.into_iter().enumerate() {
        if iteration % 1024 == 0 {
            check(cancel)?;
        }
        let edge = &candidates[entry.candidate];
        let key = (
            (edge.q[0] / bin).floor() as i64,
            (edge.q[1] / bin).floor() as i64,
        );
        let mut associated = false;
        'search: for y in key.1 - 1..=key.1 + 1 {
            for x in key.0 - 1..=key.0 + 1 {
                if let Some(list) = bins.get(&(x, y)) {
                    for &i in list {
                        diagnostics.association_checks += 1;
                        if diagnostics.association_checks.is_multiple_of(1024) {
                            check(cancel)?;
                        }
                        let ridge = &candidates[i];
                        let displacement = sub(edge.q, ridge.q);
                        if dot(edge.n, ridge.n).abs() >= COS_22
                            && dot(displacement, tangent(ridge.n)).abs() <= ridge.sigma.max(1.0)
                            && dot(displacement, ridge.n).abs() <= 2.0 * ridge.sigma
                            && distance(edge.minus, ridge.center)
                                .min(distance(edge.plus, ridge.center))
                                <= 0.10
                        {
                            associated = true;
                            break 'search;
                        }
                    }
                }
            }
        }
        if associated {
            diagnostics.associated_flanks += 1;
        } else {
            out.push(entry);
        }
    }
    Ok(out)
}

pub(super) fn reconstruct(
    reference: &Reference,
    base: &RgbaImage,
    diagnostics: &mut Diagnostics,
    cancel: &CancellationToken,
) -> Result<()> {
    let g = Geometry {
        w: base.width() as usize,
        h: base.height() as usize,
        sx: f64::from(base.width()) / f64::from(reference.source.width()),
        sy: f64::from(base.height()) / f64::from(reference.source.height()),
    };
    diagnostics.collisions = vec![0; g.w * g.h];
    let slots = transport(reference, base, g, diagnostics, cancel)?;
    let mut ridges = Vec::new();
    let mut edges = Vec::new();
    for (i, entry) in slots.iter().enumerate() {
        if i % 1024 == 0 {
            check(cancel)?;
        }
        if let Some(entry) = entry
            && entry.score >= 0.5
        {
            if reference.candidates[entry.candidate].kind == Kind::Ridge {
                ridges.push(*entry);
            } else {
                edges.push(*entry);
            }
        }
    }
    let ridge_nms = suppression(ridges, &reference.candidates, g, cancel)?;
    let (ridges, ridge_labels) = hysteresis(&ridge_nms, &reference.candidates, g, cancel)?;
    let edges = remove_flanks(edges, &ridges, &reference.candidates, diagnostics, cancel)?;
    let edge_nms = suppression(edges, &reference.candidates, g, cancel)?;
    let (edges, edge_labels) = hysteresis(&edge_nms, &reference.candidates, g, cancel)?;
    let mut owners = ridges.clone();
    for (i, edge) in edges.iter().enumerate() {
        if i % 1024 == 0 {
            check(cancel)?;
        }
        if edge.is_some() && owners[i].is_some() {
            diagnostics.owner_collisions += 1;
            diagnostics.collisions[i] = diagnostics.collisions[i].saturating_add(1);
        } else if owners[i].is_none() {
            owners[i] = *edge;
        }
    }
    for (i, a) in edges.iter().enumerate() {
        if i % 1024 == 0 {
            check(cancel)?;
        }
        if let Some(a) = a {
            for j in g.neighbors(i).filter(|&j| j > i) {
                if let Some(b) = &edges[j]
                    && compatible(a, b, &reference.candidates, g)
                    && (ridges[i].is_some() || ridges[j].is_some())
                {
                    diagnostics.collision_broken_links += 1;
                }
            }
        }
    }
    diagnostics.slots = slots;
    // Layered maps retain both kinds even where final ownership collides.
    diagnostics.nms = ridge_nms;
    diagnostics.nms.extend(edge_nms);
    diagnostics.hysteresis = ridges;
    diagnostics.hysteresis.extend(edges);
    diagnostics.components = ridge_labels;
    diagnostics.components.extend(edge_labels);
    diagnostics.owners = owners;
    check(cancel)
}
