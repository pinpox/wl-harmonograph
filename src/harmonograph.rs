use std::f64::consts::PI;

use rand::Rng;

#[derive(Clone, Copy)]
struct Pendulum {
    amplitude: f64,
    frequency: f64,
    phase: f64,
    damping: f64,
}

impl Pendulum {
    #[inline(always)]
    fn eval(&self, t: f64) -> f64 {
        self.amplitude * (t * self.frequency + self.phase).sin() * (-self.damping * t).exp()
    }
}

pub struct Harmonograph {
    x1: Pendulum,
    x2: Pendulum,
    y1: Pendulum,
    y2: Pendulum,
    t: f64,
    max_t: f64,
    step: f64,
    ring: [(f64, f64); 4],
    ring_count: u32,
}

impl Harmonograph {
    pub fn new() -> Self {
        let mut h = Self {
            x1: Pendulum {
                amplitude: 0.0,
                frequency: 0.0,
                phase: 0.0,
                damping: 0.0,
            },
            x2: Pendulum {
                amplitude: 0.0,
                frequency: 0.0,
                phase: 0.0,
                damping: 0.0,
            },
            y1: Pendulum {
                amplitude: 0.0,
                frequency: 0.0,
                phase: 0.0,
                damping: 0.0,
            },
            y2: Pendulum {
                amplitude: 0.0,
                frequency: 0.0,
                phase: 0.0,
                damping: 0.0,
            },
            t: 0.0,
            max_t: 400.0,
            step: 0.01,
            ring: [(0.0, 0.0); 4],
            ring_count: 0,
        };
        h.randomize();
        h
    }

    pub fn randomize(&mut self) {
        let mut rng = rand::thread_rng();
        let base_freq = rng.gen_range(1.0..2.0);
        let ratios: [f64; 4] = [1.0, 2.0, 3.0, 4.0];

        let pick_freq = |rng: &mut rand::rngs::ThreadRng| -> f64 {
            let ratio = ratios[rng.gen_range(0..ratios.len())];
            base_freq * ratio + rng.gen_range(-0.03..0.03)
        };

        let pendulum = |rng: &mut rand::rngs::ThreadRng, freq: f64, primary: bool| -> Pendulum {
            Pendulum {
                amplitude: if primary {
                    rng.gen_range(0.6..1.0)
                } else {
                    rng.gen_range(0.1..0.35)
                },
                frequency: freq,
                phase: rng.gen_range(0.0..2.0 * PI),
                damping: rng.gen_range(0.002..0.006),
            }
        };

        let (f_x, f_y, f_x2, f_y2) = (
            pick_freq(&mut rng),
            pick_freq(&mut rng),
            pick_freq(&mut rng),
            pick_freq(&mut rng),
        );

        self.x1 = pendulum(&mut rng, f_x, true);
        self.x2 = pendulum(&mut rng, f_x2, false);
        self.y1 = pendulum(&mut rng, f_y, true);
        self.y2 = pendulum(&mut rng, f_y2, false);
        self.t = 0.0;
        self.ring_count = 0;
    }

    #[inline(always)]
    fn eval(&self, t: f64) -> (f64, f64) {
        (
            self.x1.eval(t) + self.x2.eval(t),
            self.y1.eval(t) + self.y2.eval(t),
        )
    }

    #[inline]
    pub fn advance(&mut self) -> bool {
        if self.t > self.max_t {
            return false;
        }
        let pt = self.eval(self.t);
        self.t += self.step;

        if self.ring_count >= 4 {
            self.ring[0] = self.ring[1];
            self.ring[1] = self.ring[2];
            self.ring[2] = self.ring[3];
            self.ring[3] = pt;
        } else {
            self.ring[self.ring_count as usize] = pt;
        }
        self.ring_count += 1;
        true
    }

    #[inline]
    pub fn catmull_rom_points(&self) -> Option<&[(f64, f64); 4]> {
        if self.ring_count >= 4 {
            Some(&self.ring)
        } else {
            None
        }
    }

    /// Append triangle-strip vertices for the current Catmull-Rom segment to `verts`.
    ///
    /// Coordinates are in normalized [-1, 1] NDC. `scale_x` and `scale_y` allow
    /// aspect-ratio correction so the pattern is square regardless of screen
    /// dimensions. By appending to an existing buffer the caller can accumulate
    /// multiple simulation steps into a single continuous strip, eliminating
    /// gaps at segment joints.
    pub fn append_catmull_rom_strip(
        &self,
        scale_x: f64,
        scale_y: f64,
        line_width: f64,
        n_subdivisions: usize,
        verts: &mut Vec<[f32; 2]>,
    ) -> bool {
        let pts = match self.catmull_rom_points() {
            Some(p) => p,
            None => return false,
        };

        let p0 = (pts[0].0 * scale_x, pts[0].1 * scale_y);
        let p1 = (pts[1].0 * scale_x, pts[1].1 * scale_y);
        let p2 = (pts[2].0 * scale_x, pts[2].1 * scale_y);
        let p3 = (pts[3].0 * scale_x, pts[3].1 * scale_y);

        // Catmull-Rom → cubic Bezier control points
        let c1 = (p1.0 + (p2.0 - p0.0) / 6.0, p1.1 + (p2.1 - p0.1) / 6.0);
        let c2 = (p2.0 - (p3.0 - p1.0) / 6.0, p2.1 - (p3.1 - p1.1) / 6.0);

        let hw = line_width * 0.5;

        // When appending to an existing strip, skip t=0 because the previous
        // segment already emitted those vertices (they share the same point).
        let start = if verts.is_empty() { 0 } else { 1 };

        for i in start..=n_subdivisions {
            let t = i as f64 / n_subdivisions as f64;
            let mt = 1.0 - t;
            let mt2 = mt * mt;
            let t2 = t * t;
            let x = mt2 * mt * p1.0 + 3.0 * mt2 * t * c1.0 + 3.0 * mt * t2 * c2.0 + t2 * t * p2.0;
            let y = mt2 * mt * p1.1 + 3.0 * mt2 * t * c1.1 + 3.0 * mt * t2 * c2.1 + t2 * t * p2.1;

            // Tangent for normal computation
            let dx = -3.0 * mt2 * p1.0
                + 3.0 * (mt2 - 2.0 * mt * t) * c1.0
                + 3.0 * (2.0 * mt * t - t2) * c2.0
                + 3.0 * t2 * p2.0;
            let dy = -3.0 * mt2 * p1.1
                + 3.0 * (mt2 - 2.0 * mt * t) * c1.1
                + 3.0 * (2.0 * mt * t - t2) * c2.1
                + 3.0 * t2 * p2.1;
            let len = (dx * dx + dy * dy).sqrt().max(1e-10);
            let nx = -dy / len * hw;
            let ny = dx / len * hw;

            verts.push([(x + nx) as f32, (y + ny) as f32]);
            verts.push([(x - nx) as f32, (y - ny) as f32]);
        }

        true
    }
}
