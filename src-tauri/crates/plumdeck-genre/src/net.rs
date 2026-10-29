//! The network: input → ReLU hidden layer → two softmax heads (genre, and
//! genre/subgenre pair), trained with AdamW on mini-batches whose gradients
//! are summed across threads.

use serde::{Deserialize, Serialize};

use crate::rng::Rng;

#[derive(Clone, Debug)]
pub struct TrainConfig {
    pub hidden: usize,
    pub epochs: usize,
    pub batch: usize,
    pub learning_rate: f32,
    pub weight_decay: f32,
    /// Share of each genre held out to measure accuracy and calibrate.
    pub holdout: f32,
    /// Refit on every label after measuring.
    pub refit: bool,
    pub seed: u64,
    /// Weight of the pair head's loss.
    pub pair_weight: f32,
}

impl Default for TrainConfig {
    fn default() -> Self {
        Self { hidden: 128, epochs: 20, batch: 256, learning_rate: 2e-3, weight_decay: 1e-4, holdout: 0.2, refit: true, seed: 42, pair_weight: 1.0 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Net {
    d: usize,
    h: usize,
    g: usize,
    p: usize,
    params: Vec<f32>,
}

struct Layout {
    w1: usize,
    b1: usize,
    wg: usize,
    bg: usize,
    wp: usize,
    bp: usize,
    len: usize,
}

impl Net {
    fn layout(&self) -> Layout {
        let (d, h, g, p) = (self.d, self.h, self.g, self.p);
        let w1 = 0;
        let b1 = w1 + h * d;
        let wg = b1 + h;
        let bg = wg + g * h;
        let wp = bg + g;
        let bp = wp + p * h;
        Layout { w1, b1, wg, bg, wp, bp, len: bp + p }
    }

    pub fn valid(&self, d: usize, g: usize, p: usize) -> bool {
        self.d == d && self.g == g && self.p == p && self.h > 0 && self.params.len() == self.layout().len
    }

    fn new(d: usize, h: usize, g: usize, p: usize, rng: &mut Rng) -> Self {
        let mut net = Self { d, h, g, p, params: Vec::new() };
        let l = net.layout();
        let mut params = vec![0.0; l.len];
        let s1 = (2.0 / d as f32).sqrt();
        let s2 = (1.0 / h as f32).sqrt();
        params[l.w1..l.b1].iter_mut().for_each(|w| *w = rng.normal() * s1);
        params[l.wg..l.bg].iter_mut().for_each(|w| *w = rng.normal() * s2);
        params[l.wp..l.bp].iter_mut().for_each(|w| *w = rng.normal() * s2);
        net.params = params;
        net
    }

    fn hidden(&self, l: &Layout, x: &[f32]) -> Vec<f32> {
        let w1 = &self.params[l.w1..l.b1];
        (0..self.h)
            .map(|j| {
                let row = &w1[j * self.d..(j + 1) * self.d];
                let z = self.params[l.b1 + j] + row.iter().zip(x).map(|(w, v)| w * v).sum::<f32>();
                z.max(0.0)
            })
            .collect()
    }

    fn head(&self, w: usize, b: usize, n: usize, hidden: &[f32]) -> Vec<f32> {
        (0..n)
            .map(|k| {
                let row = &self.params[w + k * self.h..w + (k + 1) * self.h];
                self.params[b + k] + row.iter().zip(hidden).map(|(w, v)| w * v).sum::<f32>()
            })
            .collect()
    }

    /// Logits of the genre head and the pair head.
    pub fn forward(&self, x: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let l = self.layout();
        let hidden = self.hidden(&l, x);
        (self.head(l.wg, l.bg, self.g, &hidden), self.head(l.wp, l.bp, self.p, &hidden))
    }

    /// Add one sample's gradient to `grad`.
    fn accumulate(&self, l: &Layout, x: &[f32], genre: usize, pair: Option<usize>, pair_weight: f32, grad: &mut [f32]) {
        let hidden = self.hidden(l, x);
        let mut dh = vec![0.0f32; self.h];
        let mut backprop_head = |w: usize, b: usize, n: usize, label: usize, weight: f32, grad: &mut [f32]| {
            let logits = self.head(w, b, n, &hidden);
            let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let mut probs: Vec<f32> = logits.iter().map(|v| (v - max).exp()).collect();
            let sum: f32 = probs.iter().sum();
            for (k, prob) in probs.iter_mut().enumerate() {
                *prob /= sum;
                let delta = weight * (*prob - if k == label { 1.0 } else { 0.0 });
                grad[b + k] += delta;
                let row = w + k * self.h;
                for j in 0..self.h {
                    grad[row + j] += delta * hidden[j];
                    dh[j] += delta * self.params[row + j];
                }
            }
        };
        backprop_head(l.wg, l.bg, self.g, genre, 1.0, grad);
        if let Some(pair) = pair {
            backprop_head(l.wp, l.bp, self.p, pair, pair_weight, grad);
        }
        for j in 0..self.h {
            if hidden[j] <= 0.0 || dh[j] == 0.0 {
                continue;
            }
            grad[l.b1 + j] += dh[j];
            let row = &mut grad[l.w1 + j * self.d..l.w1 + (j + 1) * self.d];
            for (g, v) in row.iter_mut().zip(x) {
                *g += dh[j] * v;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn train(
        x: &[Vec<f32>],
        genres: &[usize],
        pairs: &[Option<usize>],
        genre_count: usize,
        pair_count: usize,
        config: &TrainConfig,
        rng: &mut Rng,
        progress: &(dyn Fn(f32) + Sync),
    ) -> Self {
        let d = x.first().map_or(0, Vec::len);
        let mut net = Self::new(d, config.hidden.max(1), genre_count, pair_count, rng);
        let l = net.layout();
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8);
        let (mut m, mut v) = (vec![0.0f32; l.len], vec![0.0f32; l.len]);
        let (beta1, beta2, eps) = (0.9f32, 0.999f32, 1e-8f32);
        let mut step = 0i32;
        let mut order: Vec<usize> = (0..x.len()).collect();
        let batch = config.batch.max(1);
        let epochs = config.epochs.max(1);
        for epoch in 0..epochs {
            rng.shuffle(&mut order);
            // Cosine decay to 5 % of the initial rate.
            let lr = config.learning_rate * (0.05 + 0.95 * 0.5 * (1.0 + (std::f32::consts::PI * epoch as f32 / epochs as f32).cos()));
            for chunk in order.chunks(batch) {
                let per = chunk.len().div_ceil(threads);
                let parts: Vec<Vec<f32>> = std::thread::scope(|scope| {
                    let net = &net;
                    let l = &l;
                    let handles: Vec<_> = chunk
                        .chunks(per)
                        .map(|part| {
                            scope.spawn(move || {
                                let mut grad = vec![0.0f32; l.len];
                                for i in part {
                                    net.accumulate(l, &x[*i], genres[*i], pairs[*i], config.pair_weight, &mut grad);
                                }
                                grad
                            })
                        })
                        .collect();
                    handles.into_iter().map(|h| h.join().expect("training thread")).collect()
                });
                let scale = 1.0 / chunk.len() as f32;
                step += 1;
                let (c1, c2) = (1.0 - beta1.powi(step), 1.0 - beta2.powi(step));
                for k in 0..l.len {
                    let g = parts.iter().map(|part| part[k]).sum::<f32>() * scale;
                    m[k] = beta1 * m[k] + (1.0 - beta1) * g;
                    v[k] = beta2 * v[k] + (1.0 - beta2) * g * g;
                    let is_weight = k < l.b1 || (l.wg..l.bg).contains(&k) || (l.wp..l.bp).contains(&k);
                    let decay = if is_weight { config.weight_decay * net.params[k] } else { 0.0 };
                    net.params[k] -= lr * ((m[k] / c1) / ((v[k] / c2).sqrt() + eps) + decay);
                }
            }
            progress((epoch + 1) as f32 / epochs as f32);
        }
        net
    }
}
