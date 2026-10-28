//! Python `utils/array_codec.py` storage format. macOS and Windows targets use
//! little-endian float32; decoded values widen to f64 just like NumPy `.tolist()`.

pub const WAVEFORM_POINTS: usize = 500;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ArrayCodecError {
    #[error("waveform points must be positive")]
    ZeroPoints,
    #[error("buffer size must be a multiple of element size")]
    InvalidFloatBuffer,
}

pub fn pack_u8_waveform(values: Option<&[f64]>, points: usize) -> Result<Option<Vec<u8>>, ArrayCodecError> {
    let Some(values) = values else { return Ok(None) };
    if values.is_empty() {
        return Ok(Some(Vec::new()));
    }
    if points == 0 {
        return Err(ArrayCodecError::ZeroPoints);
    }
    let clean: Vec<f64> = values
        .iter()
        .map(|&value| {
            if value.is_nan() || value == f64::NEG_INFINITY {
                0.0
            } else if value == f64::INFINITY {
                1.0
            } else {
                value
            }
        })
        .collect();
    let quantize = |value: f64| (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let packed = if clean.len() > points {
        // The Python codec drops the trailing partial chunk. It also takes
        // absolute values only when downsampling, not for short waveforms.
        let chunk = clean.len() / points;
        clean
            .chunks_exact(chunk)
            .take(points)
            .map(|values| quantize(values.iter().map(|v| v.abs()).fold(0.0, f64::max)))
            .collect()
    } else {
        clean.into_iter().map(quantize).collect()
    };
    Ok(Some(packed))
}

pub fn unpack_u8_waveform(blob: Option<&[u8]>) -> Vec<f64> {
    blob.unwrap_or_default()
        .iter()
        .map(|&value| (f32::from(value) / 255.0_f32) as f64)
        .collect()
}

pub fn pack_f32(values: Option<&[f64]>) -> Option<Vec<u8>> {
    values.map(|values| {
        values
            .iter()
            .flat_map(|&value| {
                // Cast before cleaning: finite f64 values may overflow to f32 Inf.
                let value = value as f32;
                (if value.is_finite() { value } else { 0.0 }).to_le_bytes()
            })
            .collect()
    })
}

pub fn unpack_f32(blob: Option<&[u8]>) -> Result<Vec<f64>, ArrayCodecError> {
    let bytes = blob.unwrap_or_default();
    if bytes.len() % 4 != 0 {
        return Err(ArrayCodecError::InvalidFloatBuffer);
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().expect("four-byte chunk")) as f64)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_empty_and_invalid_buffers_match_python() {
        assert_eq!(pack_f32(None), None);
        assert_eq!(pack_f32(Some(&[])), Some(vec![]));
        assert_eq!(pack_u8_waveform(None, 0).unwrap(), None);
        assert_eq!(pack_u8_waveform(Some(&[]), 0).unwrap(), Some(vec![]));
        assert_eq!(
            pack_u8_waveform(Some(&[0.5]), 0),
            Err(ArrayCodecError::ZeroPoints)
        );
        assert!(unpack_f32(None).unwrap().is_empty());
        assert!(unpack_u8_waveform(None).is_empty());
        assert_eq!(
            unpack_f32(Some(&[0, 1, 2])),
            Err(ArrayCodecError::InvalidFloatBuffer)
        );
    }

    #[test]
    fn waveform_clips_and_cleans_before_rounding() {
        let values = [
            -2.0,
            -0.0,
            0.0,
            0.1,
            0.5,
            1.0,
            2.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];
        assert_eq!(
            pack_u8_waveform(Some(&values), 500).unwrap().unwrap(),
            [0, 0, 0, 26, 128, 255, 255, 0, 255, 0]
        );
        assert_eq!(
            unpack_u8_waveform(Some(&[1, 26, 128, 255])),
            [0.003921568859368563, 0.10196078568696976, 0.501960813999176, 1.0]
        );
    }

    #[test]
    fn waveform_downsampling_preserves_python_absolute_max_and_tail_drop() {
        assert_eq!(
            pack_u8_waveform(Some(&[-0.8, 0.1, 0.2, -0.4, 0.99]), 2)
                .unwrap()
                .unwrap(),
            [204, 102]
        );
        // len > points but chunk size one: only the first points survive.
        assert_eq!(
            pack_u8_waveform(Some(&[-0.8, 0.1, 1.0]), 2).unwrap().unwrap(),
            [204, 26]
        );
        assert_eq!(
            pack_u8_waveform(Some(&[f64::NEG_INFINITY, -0.5]), 1)
                .unwrap()
                .unwrap(),
            [128]
        );
    }

    #[test]
    fn f32_blob_bytes_and_widening_match_numpy() {
        let values = [
            0.0,
            -0.0,
            0.1,
            127.9,
            1.0e40,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];
        let blob = pack_f32(Some(&values)).unwrap();
        assert_eq!(
            &blob[..16],
            &[0, 0, 0, 0, 0, 0, 0, 128, 205, 204, 204, 61, 205, 204, 255, 66]
        );
        let result = unpack_f32(Some(&blob)).unwrap();
        assert_eq!(
            result,
            [
                0.0,
                -0.0,
                0.10000000149011612,
                127.9000015258789,
                0.0,
                0.0,
                0.0,
                0.0
            ]
        );
        assert!(result[1].is_sign_negative());
        assert!(unpack_f32(Some(&f32::NAN.to_le_bytes())).unwrap()[0].is_nan());
    }
}
