//! Replay the real Python/NumPy codec's outputs, including non-finite values.
use std::path::PathBuf;

use plumdeck_db::array_codec::{pack_f32, pack_u8_waveform, unpack_f32, unpack_u8_waveform};
use serde_json::Value;

fn floats(value: &Value) -> Option<Vec<f64>> {
    value.as_array().map(|values| {
        values
            .iter()
            .map(|v| f64::from_bits(u64::from_str_radix(v.as_str().unwrap(), 16).unwrap()))
            .collect()
    })
}

fn blob(value: &Value) -> Option<Vec<u8>> {
    value.as_str().map(|text| {
        text.as_bytes()
            .chunks_exact(2)
            .map(|digits| u8::from_str_radix(std::str::from_utf8(digits).unwrap(), 16).unwrap())
            .collect()
    })
}

fn assert_floats(actual: &[f64], expected: &[f64], line: usize) {
    assert_eq!(actual.len(), expected.len(), "line {line}");
    for (index, (&a, &b)) in actual.iter().zip(expected).enumerate() {
        assert!(
            a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()),
            "line {line}, value {index}: {a:?} != {b:?}"
        );
    }
}

#[test]
fn array_storage_matches_python_numpy() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../contracts/compat-vectors/array_codec.jsonl");
    let text = std::fs::read_to_string(path).unwrap();
    let mut count = 0;
    for (line, text) in text.lines().enumerate() {
        let case: Value = serde_json::from_str(text).unwrap();
        count += 1;
        match case["op"].as_str().unwrap() {
            "pack_f32" => assert_eq!(
                pack_f32(floats(&case["values"]).as_deref()),
                blob(&case["out"]),
                "line {line}"
            ),
            "pack_u8" => {
                let result = pack_u8_waveform(
                    floats(&case["values"]).as_deref(),
                    case["points"].as_u64().unwrap() as usize,
                );
                if case.get("error").is_some() {
                    assert!(result.is_err(), "line {line}");
                } else {
                    assert_eq!(result.unwrap(), blob(&case["out"]), "line {line}");
                }
            }
            "unpack_u8" => assert_floats(
                &unpack_u8_waveform(blob(&case["blob"]).as_deref()),
                &floats(&case["out"]).unwrap(),
                line,
            ),
            "unpack_f32" => {
                let result = unpack_f32(blob(&case["blob"]).as_deref());
                if case.get("error").is_some() {
                    assert!(result.is_err(), "line {line}");
                } else {
                    assert_floats(&result.unwrap(), &floats(&case["out"]).unwrap(), line);
                }
            }
            op => panic!("unknown operation {op}"),
        }
    }
    assert!(count >= 500, "expected the complete seeded corpus, got {count}");
}
