use super::*;

pub(super) fn gaussian(
    p: &[P],
    w: usize,
    h: usize,
    channel: usize,
    sigma: f64,
    cancel: &CancellationToken,
) -> Result<Vec<[f64; 1]>> {
    let r = (3.0 * sigma).ceil() as isize;
    let mut kernel: Vec<f64> = (-r..=r)
        .map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f64 = kernel.iter().sum();
    if !sum.is_finite() || sum.abs() < 1e-12 {
        return Err(Error::Numerical);
    }
    for v in &mut kernel {
        *v /= sum;
    }
    let mut tmp = vec![0.0; w * h];
    let mut out = vec![[0.0]; w * h];
    for y in 0..h {
        check(cancel)?;
        for x in 0..w {
            if x % 256 == 0 {
                check(cancel)?;
            }
            for (k, weight) in kernel.iter().enumerate() {
                let xx = (x as isize + k as isize - r).clamp(0, w as isize - 1) as usize;
                tmp[y * w + x] += p[y * w + xx][channel] * weight;
            }
        }
    }
    for y in 0..h {
        check(cancel)?;
        for x in 0..w {
            if x % 256 == 0 {
                check(cancel)?;
            }
            for (k, weight) in kernel.iter().enumerate() {
                let yy = (y as isize + k as isize - r).clamp(0, h as isize - 1) as usize;
                out[y * w + x][0] += tmp[yy * w + x] * weight;
            }
        }
    }
    Ok(out)
}

pub(super) fn derivatives(f: &[[f64; 1]], w: usize, h: usize, x: usize, y: usize) -> (V, [f64; 3]) {
    let l = x.saturating_sub(1);
    let r = (x + 1).min(w - 1);
    let u = y.saturating_sub(1);
    let d = (y + 1).min(h - 1);
    let at = |x: usize, y: usize| f[y * w + x][0];
    let c = at(x, y);
    (
        [(at(r, y) - at(l, y)) * 0.5, (at(x, d) - at(x, u)) * 0.5],
        [
            at(r, y) - 2.0 * c + at(l, y),
            (at(r, d) - at(l, d) - at(r, u) + at(l, u)) * 0.25,
            at(x, d) - 2.0 * c + at(x, u),
        ],
    )
}

fn eigen(h: [f64; 3]) -> Option<(f64, f64, V)> {
    let [a, b, d] = h;
    let mid = (a + d) * 0.5;
    let radius = ((a - d) * 0.5).hypot(b);
    let hi = mid + radius;
    let lo = mid - radius;
    let (normal, other) = if hi.abs() >= lo.abs() {
        (hi, lo)
    } else {
        (lo, hi)
    };
    if normal.abs() <= 1e-12 {
        return None;
    }
    let n = if b == 0.0 {
        if normal == a { [1.0, 0.0] } else { [0.0, 1.0] }
    } else {
        let v1 = [b, normal - a];
        let v2 = [normal - d, b];
        norm(if dot(v1, v1) >= dot(v2, v2) { v1 } else { v2 })?
    };
    let n = if n[0] < 0.0 || (n[0] == 0.0 && n[1] < 0.0) {
        [-n[0], -n[1]]
    } else {
        n
    };
    Some((normal, other, n))
}

struct Proposal {
    kind: Kind,
    q: V,
    n: V,
    response: f64,
    polarity: i8,
}
struct Field<'a> {
    p: &'a [P],
    w: usize,
    h: usize,
    sigma: f64,
    channel: usize,
}
impl Field<'_> {
    fn candidate(&self, id: usize, proposal: Proposal) -> Option<Candidate> {
        let Proposal {
            kind,
            q,
            n,
            response,
            polarity,
        } = proposal;
        let d = (2.0 * self.sigma).max(1.0);
        let minus_q = offset(q, n, -d);
        let plus_q = offset(q, n, d);
        if !inside(minus_q, self.w, self.h) || !inside(plus_q, self.w, self.h) {
            return None;
        }
        let center = sample(self.p, self.w, self.h, q);
        let minus = sample(self.p, self.w, self.h, minus_q);
        let plus = sample(self.p, self.w, self.h, plus_q);
        let c = center[self.channel];
        let m = minus[self.channel];
        let p = plus[self.channel];
        if kind == Kind::Ridge {
            let dm = c - m;
            let dp = c - p;
            if dm * dp <= 0.0
                || (polarity > 0 && dm >= 0.0)
                || (polarity < 0 && dm <= 0.0)
                || dm.abs().min(dp.abs()) < 0.25 * dm.abs().max(dp.abs())
                || distance(center, minus) <= distance(minus, plus)
                || distance(center, plus) <= distance(minus, plus)
            {
                return None;
            }
        }
        let contrast = c.max(m).max(p) - c.min(m).min(p);
        let z = response / (0.25 * contrast).max(0.02);
        if !z.is_finite() || z < 0.5 {
            return None;
        }
        Some(Candidate {
            id,
            kind,
            q,
            n,
            sigma: self.sigma,
            channel: self.channel,
            polarity,
            response,
            contrast,
            z,
            effective_z: z,
            direction: None,
            center,
            minus,
            plus,
            colors: [
                nearest(q, self.w, self.h),
                nearest(minus_q, self.w, self.h),
                nearest(plus_q, self.w, self.h),
            ],
        })
    }
}

pub(super) fn detect(
    p: &[P],
    w: usize,
    h: usize,
    settings: &Settings,
    cancel: &CancellationToken,
) -> Result<(Vec<Candidate>, Vec<usize>)> {
    let mut candidates = Vec::new();
    for (scale_index, &sigma) in settings.scales.iter().enumerate() {
        let planes = if settings.measure_direction || settings.confidence_gate {
            let mut planes = Vec::with_capacity(4);
            for channel in 0..4 {
                planes.push(gaussian(p, w, h, channel, sigma, cancel)?);
            }
            Some(planes)
        } else {
            None
        };
        let direction = if let Some(planes) = &planes {
            Some(confidence::matrix(planes, w, h, sigma, cancel)?)
        } else {
            None
        };
        for channel in 0..4 {
            check(cancel)?;
            let scratch;
            let f = if let Some(planes) = &planes {
                &planes[channel]
            } else {
                scratch = gaussian(p, w, h, channel, sigma, cancel)?;
                &scratch
            };
            let mut magnitude = vec![[0.0]; w * h];
            for y in 0..h {
                check(cancel)?;
                for x in 0..w {
                    if x % 256 == 0 {
                        check(cancel)?;
                    }
                    let (g, _) = derivatives(f, w, h, x, y);
                    magnitude[y * w + x][0] = g[0].hypot(g[1]);
                }
            }
            let field = Field {
                p,
                w,
                h,
                sigma,
                channel,
            };
            for y in 0..h {
                check(cancel)?;
                for x in 0..w {
                    if x % 256 == 0 {
                        check(cancel)?;
                    }
                    let id =
                        (((y * w + x) * settings.scales.len() + scale_index) * 4 + channel) * 2;
                    let q = [x as f64, y as f64];
                    let (g, hessian) = derivatives(f, w, h, x, y);
                    let mut proposals = [None, None];
                    if let Some((ln, lt, n)) = eigen(hessian) {
                        let anisotropy = 1.0 - lt.abs() / ln.abs();
                        let delta = -dot(g, n) / ln;
                        if anisotropy >= 0.6
                            && (delta * n[0]).abs() <= 0.5
                            && (delta * n[1]).abs() <= 0.5
                        {
                            proposals[0] = field.candidate(
                                id,
                                Proposal {
                                    kind: Kind::Ridge,
                                    q: offset(q, n, delta),
                                    n,
                                    response: sigma * sigma * ln.abs() * anisotropy,
                                    polarity: if ln > 0.0 { 1 } else { -1 },
                                },
                            );
                        }
                    }
                    let m = magnitude[y * w + x][0];
                    if m > 0.0 {
                        let n = [g[0] / m, g[1] / m];
                        let minus = sample(&magnitude, w, h, offset(q, n, -1.0))[0];
                        let plus = sample(&magnitude, w, h, offset(q, n, 1.0))[0];
                        if m >= minus && m > plus {
                            let denom = minus - 2.0 * m + plus;
                            let delta = if denom >= -1e-12 {
                                0.0
                            } else {
                                (0.5 * (minus - plus) / denom).clamp(-0.5, 0.5)
                            };
                            proposals[1] = field.candidate(
                                id + 1,
                                Proposal {
                                    kind: Kind::Edge,
                                    q: offset(q, n, delta),
                                    n,
                                    response: sigma * m,
                                    polarity: 1,
                                },
                            );
                        }
                    }
                    for mut proposal in proposals.into_iter().flatten() {
                        if let Some(matrix) = &direction {
                            let evidence = confidence::direction(
                                sample(matrix, w, h, proposal.q),
                                proposal.n,
                            )?;
                            if settings.confidence_gate && !evidence.confident {
                                proposal.effective_z = proposal.z.min(0.75);
                            }
                            proposal.direction = Some(evidence);
                        }
                        if candidates.len() == settings.candidate_limit {
                            return Err(Error::Resource(settings.memory_budget));
                        }
                        if candidates.len() == candidates.capacity() {
                            let additional =
                                (settings.candidate_limit - candidates.len()).min(4096);
                            candidates
                                .try_reserve_exact(additional)
                                .map_err(|_| Error::Resource(settings.memory_budget))?;
                        }
                        candidates.push(proposal);
                    }
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    sort(
        &mut order,
        |&a, &b| {
            candidates[b]
                .effective_z
                .total_cmp(&candidates[a].effective_z)
                .then(candidates[a].id.cmp(&candidates[b].id))
        },
        cancel,
    )?;
    let mut bins: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    let mut kept = Vec::new();
    for (iteration, i) in order.into_iter().enumerate() {
        if iteration % 1024 == 0 {
            check(cancel)?;
        }
        let c = &candidates[i];
        let key = (c.q[0].floor() as i64, c.q[1].floor() as i64);
        let mut duplicate = false;
        for yy in key.1 - 1..=key.1 + 1 {
            for xx in key.0 - 1..=key.0 + 1 {
                if let Some(list) = bins.get(&(xx, yy)) {
                    for &j in list {
                        let r = &candidates[j];
                        let d = sub(c.q, r.q);
                        if c.kind == r.kind
                            && dot(d, d) <= 0.75 * 0.75
                            && dot(c.n, r.n).abs() >= COS_22
                            && distance(c.center, r.center) <= 0.10
                        {
                            duplicate = true;
                            break;
                        }
                    }
                }
            }
        }
        if !duplicate {
            bins.entry(key).or_default().push(i);
            kept.push(i);
        }
    }
    check(cancel)?;
    Ok((candidates, kept))
}
