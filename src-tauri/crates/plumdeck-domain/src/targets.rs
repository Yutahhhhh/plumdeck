//! `sanitize_target_parameters` (backend/domain/services/target_parameters.py):
//! the structured audio-feature targets the recommenders accept.

/// Finite, clamped targets. `bpm` is 1..=300, the features 0..=1, the years
/// 1900..=2100 (as floats, swapped when reversed). `max_tempo_distance` is
/// never produced by sanitizing; callers that narrow the tempo window set it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Targets {
    pub bpm: Option<f64>,
    pub energy: Option<f64>,
    pub danceability: Option<f64>,
    pub brightness: Option<f64>,
    pub noisiness: Option<f64>,
    pub year_min: Option<f64>,
    pub year_max: Option<f64>,
    pub max_tempo_distance: Option<f64>,
}

/// The raw values a caller supplied (each already a Python number).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RawTargets {
    pub bpm: Option<f64>,
    pub energy: Option<f64>,
    pub danceability: Option<f64>,
    pub brightness: Option<f64>,
    pub noisiness: Option<f64>,
    pub year_min: Option<f64>,
    pub year_max: Option<f64>,
}

impl Targets {
    /// The four vibe features, in the order the builders read them.
    pub fn features(&self) -> [(&'static str, Option<f64>); 4] {
        [
            ("energy", self.energy),
            ("danceability", self.danceability),
            ("brightness", self.brightness),
            ("noisiness", self.noisiness),
        ]
    }
}

fn year(value: f64) -> f64 {
    // float(max(1900, min(2100, int(number))))
    value.trunc().clamp(1900.0, 2100.0)
}

/// `sanitize_target_parameters`.
pub fn sanitize(raw: &RawTargets) -> Targets {
    let finite = |value: Option<f64>| value.filter(|number| number.is_finite());
    let unit = |value: Option<f64>| finite(value).map(|number| number.clamp(0.0, 1.0));
    let mut targets = Targets {
        bpm: finite(raw.bpm).map(|number| number.clamp(1.0, 300.0)),
        energy: unit(raw.energy),
        danceability: unit(raw.danceability),
        brightness: unit(raw.brightness),
        noisiness: unit(raw.noisiness),
        year_min: finite(raw.year_min).map(year),
        year_max: finite(raw.year_max).map(year),
        max_tempo_distance: None,
    };
    if let (Some(low), Some(high)) = (targets.year_min, targets.year_max) {
        if low > high {
            targets.year_min = Some(high);
            targets.year_max = Some(low);
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_and_drops_non_finite_values() {
        let raw = RawTargets {
            bpm: Some(500.0),
            energy: Some(f64::NAN),
            danceability: Some(-1.0),
            brightness: Some(0.5),
            noisiness: None,
            year_min: Some(2200.7),
            year_max: Some(1999.9),
        };
        let targets = sanitize(&raw);
        assert_eq!(targets.bpm, Some(300.0));
        assert_eq!(targets.energy, None);
        assert_eq!(targets.danceability, Some(0.0));
        assert_eq!((targets.year_min, targets.year_max), (Some(1999.0), Some(2100.0)));
        assert_eq!(sanitize(&RawTargets::default()), Targets::default());
    }
}
