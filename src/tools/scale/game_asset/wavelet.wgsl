struct Params {
    width: u32, height: u32, source_width: u32, source_height: u32,
    pad: u32, length: u32, inverse: u32, axis: u32,
    tangent: f32, wavelength: f32, bandwidth: f32, orientations: f32,
    weights: vec3<f32>, energy_offset: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec2<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec2<f32>>;
@group(0) @binding(2) var<uniform> p: Params;
@group(0) @binding(3) var<storage, read> twiddles: array<vec2<f32>>;
var<workgroup> values: array<vec2<f32>, 2048>;

fn multiply(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
}
fn address(line: u32, i: u32, channel: u32) -> u32 {
    return channel * p.width * p.height
        + select(line * p.width + i, i * p.width + line, p.axis == 1u);
}

// Each workgroup owns one complete line. Every butterfly has one writer;
// barriers separate stages. Twiddles are computed in f64 on the host once,
// avoiding GPU trigonometric error inside the repeated FFT butterflies.
@compute @workgroup_size(256)
fn fft(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let bits = 31u - countLeadingZeros(p.length);
    for (var i = lane; i < p.length; i += 256u) {
        var reversed = 0u;
        if (p.length > 1u) { reversed = reverseBits(i) >> (32u - bits); }
        values[reversed] = input[address(group.x, i, group.y)];
    }
    workgroupBarrier();
    for (var span = 2u; span <= p.length; span *= 2u) {
        let half = span / 2u;
        for (var i = lane; i < p.length / 2u; i += 256u) {
            let j = i % half;
            let base = (i / half) * span + j;
            var w = twiddles[j * (2048u / span)];
            if (p.inverse != 0u) { w.y = -w.y; }
            let a = values[base];
            let b = multiply(values[base + half], w);
            values[base] = a + b;
            values[base + half] = a - b;
        }
        workgroupBarrier();
    }
    for (var i = lane; i < p.length; i += 256u) {
        var z = values[i];
        if (p.inverse != 0u && p.axis == 1u) {
            z *= 1.0 / f32(p.width * p.height);
        }
        output[address(group.x, i, group.y)] = z;
    }
}

@compute @workgroup_size(256)
fn apply_filter(@builtin(global_invocation_id) id: vec3<u32>) {
    let n = p.width * p.height;
    if (id.x >= n) { return; }
    let x = id.x % p.width;
    let y = id.x / p.width;
    let fx = select(f32(x) - f32(p.width), f32(x), x <= p.width / 2u) / f32(p.width);
    let fy = select(f32(y) - f32(p.height), f32(y), y <= p.height / 2u) / f32(p.height);
    let radius = length(vec2<f32>(fx, fy));
    var gain = 0.0;
    if (radius > 0.0) {
        let pi = 3.141592653589793;
        let angle0 = atan2(fy, fx) - (p.tangent + pi / 2.0) + pi;
        let angle = angle0 - floor(angle0 / (2.0 * pi)) * (2.0 * pi) - pi;
        let angular = 0.5 * (1.0 + cos(min(abs(angle) * p.orientations / 2.0, pi)));
        let logarithm = log(radius * p.wavelength);
        let bandwidth = log(p.bandwidth);
        let radial = exp(-(logarithm * logarithm) / (2.0 * bandwidth * bandwidth));
        let r = radius / 0.4;
        let r2 = r * r;
        let r4 = r2 * r2;
        let r8 = r4 * r4;
        let r16 = r8 * r8;
        gain = radial * angular / (1.0 + r16 * r4);
    }
    for (var channel = 0u; channel < 3u; channel++) {
        output[channel * n + id.x] = input[channel * n + id.x] * gain;
    }
}

@compute @workgroup_size(256)
fn energy(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.source_width * p.source_height) { return; }
    let x = id.x % p.source_width;
    let y = id.x / p.source_width;
    let location = (y + p.pad) * p.width + x + p.pad;
    var sum = vec2<f32>(0.0);
    for (var channel = 0u; channel < 3u; channel++) {
        let z = input[channel * p.width * p.height + location];
        sum += p.weights[channel] * z * z;
    }
    output[p.energy_offset + id.x] = sum;
}
