//! Small, deliberately strict reader for the F16 GGUF models Diorama ships.
//!
//! GGUF offsets are relative to the aligned tensor-data section.  Keeping the
//! file on disk and reading checked ranges avoids a second 420 MiB allocation
//! before Burn uploads an individual tensor to its backend.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

const MAGIC: u32 = 0x4655_4747; // "GGUF" as a little-endian u32
const VERSION: u32 = 3;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ENTRIES: u64 = 100_000;
const MAX_STRING_BYTES: usize = 1024 * 1024;
const MAX_TENSOR_BYTES: u64 = 512 * 1024 * 1024;
const F32: u32 = 0;
const F16: u32 = 1;
const I64: u32 = 27;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MetadataValue {
    String(String),
    I64(i64),
    U64(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TensorInfo {
    pub(crate) name: String,
    /// GGUF's native (least-significant axis first) dimensions.
    pub(crate) dimensions: Vec<u64>,
    pub(crate) dtype: u32,
    pub(crate) offset: u64,
    pub(crate) byte_len: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct GgufModel {
    path: PathBuf,
    data_offset: u64,
    file_len: u64,
    metadata: BTreeMap<String, MetadataValue>,
    tensors: BTreeMap<String, TensorInfo>,
}

impl GgufModel {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path)
            .map_err(|error| format!("cannot open GGUF model {}: {error}", path.display()))?;
        let file_len = file
            .metadata()
            .map_err(|error| format!("cannot stat GGUF model {}: {error}", path.display()))?
            .len();
        if file_len > MAX_FILE_BYTES {
            return Err(format!(
                "GGUF model {} exceeds {MAX_FILE_BYTES}-byte safety limit",
                path.display()
            ));
        }
        let magic = read_u32(&mut file)?;
        if magic != MAGIC {
            return Err(format!("{} is not a GGUF file", path.display()));
        }
        let version = read_u32(&mut file)?;
        if version != VERSION {
            return Err(format!(
                "unsupported GGUF version {version}; expected {VERSION}"
            ));
        }
        let tensor_count = read_u64(&mut file)?;
        let metadata_count = read_u64(&mut file)?;
        if tensor_count > MAX_ENTRIES || metadata_count > MAX_ENTRIES {
            return Err("GGUF has an unreasonable tensor or metadata entry count".into());
        }

        let mut metadata = BTreeMap::new();
        for _ in 0..metadata_count {
            let key = read_string(&mut file)?;
            if metadata
                .insert(key.clone(), read_metadata_value(&mut file)?)
                .is_some()
            {
                return Err(format!("duplicate GGUF metadata key {key:?}"));
            }
        }
        let alignment = match metadata.get("general.alignment") {
            None => 32,
            Some(MetadataValue::U64(value)) => *value,
            Some(MetadataValue::I64(value)) if *value > 0 => *value as u64,
            _ => return Err("GGUF general.alignment must be a positive integer".into()),
        };
        if alignment == 0 || !alignment.is_power_of_two() || alignment > 4096 {
            return Err(format!("invalid GGUF tensor alignment {alignment}"));
        }

        let mut raw_tensors = Vec::with_capacity(tensor_count as usize);
        for _ in 0..tensor_count {
            let name = read_string(&mut file)?;
            let dimensions_count = read_u32(&mut file)?;
            // Fused BatchNorm exports retain scalar `num_batches_tracked`
            // tensors. They are unused at inference, but must be parsed to
            // locate the following tensor data accurately.
            if dimensions_count > 4 {
                return Err(format!(
                    "GGUF tensor {name:?} has unsupported rank {dimensions_count}"
                ));
            }
            let mut dimensions = Vec::with_capacity(dimensions_count as usize);
            let mut elements = 1_u64;
            for _ in 0..dimensions_count {
                let dimension = read_u64(&mut file)?;
                if dimension == 0 {
                    return Err(format!("GGUF tensor {name:?} has a zero dimension"));
                }
                elements = elements
                    .checked_mul(dimension)
                    .ok_or_else(|| format!("GGUF tensor {name:?} element count overflows"))?;
                dimensions.push(dimension);
            }
            let dtype = read_u32(&mut file)?;
            // Current exports retain scalar BatchNorm bookkeeping in I64;
            // it has no inference role, but parsing it is necessary for all
            // subsequent F16 tensor offsets to remain correct.
            if !(matches!(dtype, F32 | F16) || dtype == I64 && dimensions_count == 0) {
                return Err(format!("GGUF tensor {name:?} has unsupported type {dtype}"));
            }
            let offset = read_u64(&mut file)?;
            let element_size = match dtype {
                F32 => 4,
                F16 => 2,
                I64 => 8,
                _ => unreachable!(),
            };
            let byte_len = elements
                .checked_mul(element_size)
                .ok_or_else(|| format!("GGUF tensor {name:?} byte length overflows"))?;
            if byte_len > MAX_TENSOR_BYTES {
                return Err(format!(
                    "GGUF tensor {name:?} exceeds {MAX_TENSOR_BYTES}-byte safety limit"
                ));
            }
            raw_tensors.push(TensorInfo {
                name,
                dimensions,
                dtype,
                offset,
                byte_len,
            });
        }
        let data_offset = align_up(file.stream_position().map_err(io_error)?, alignment)?;
        if data_offset > file_len {
            return Err("GGUF tensor-data section starts past end of file".into());
        }
        let mut tensors = BTreeMap::new();
        for tensor in raw_tensors {
            let start = data_offset
                .checked_add(tensor.offset)
                .ok_or_else(|| format!("GGUF tensor {:?} offset overflows", tensor.name))?;
            let end = start
                .checked_add(tensor.byte_len)
                .ok_or_else(|| format!("GGUF tensor {:?} range overflows", tensor.name))?;
            if end > file_len {
                return Err(format!(
                    "GGUF tensor {:?} lies outside the model file",
                    tensor.name
                ));
            }
            if tensors.insert(tensor.name.clone(), tensor).is_some() {
                return Err("duplicate GGUF tensor name".into());
            }
        }
        let model = Self {
            path: path.to_owned(),
            data_offset,
            file_len,
            metadata,
            tensors,
        };
        model.validate_birefnet()?;
        Ok(model)
    }

    pub(crate) fn metadata_i64(&self, key: &str) -> Result<i64, String> {
        match self.metadata.get(key) {
            Some(MetadataValue::I64(value)) => Ok(*value),
            Some(MetadataValue::U64(value)) => {
                i64::try_from(*value).map_err(|_| format!("GGUF metadata {key:?} does not fit i64"))
            }
            _ => Err(format!("missing or non-integer GGUF metadata {key:?}")),
        }
    }

    pub(crate) fn metadata_string(&self, key: &str) -> Result<&str, String> {
        match self.metadata.get(key) {
            Some(MetadataValue::String(value)) => Ok(value),
            _ => Err(format!("missing or non-string GGUF metadata {key:?}")),
        }
    }

    #[cfg(test)]
    pub(crate) fn tensor(
        &self,
        name: &str,
        expected_dimensions: &[u64],
    ) -> Result<&TensorInfo, String> {
        let tensor = self
            .tensors
            .get(name)
            .ok_or_else(|| format!("required GGUF tensor {name:?} is absent"))?;
        if tensor.dimensions != expected_dimensions {
            return Err(format!(
                "GGUF tensor {name:?} dimensions {:?} differ from expected {expected_dimensions:?}",
                tensor.dimensions
            ));
        }
        Ok(tensor)
    }

    pub(crate) fn named_tensor(&self, name: &str) -> Result<&TensorInfo, String> {
        self.tensors
            .get(name)
            .ok_or_else(|| format!("required GGUF tensor {name:?} is absent"))
    }

    pub(crate) fn tensor_f32(&self, tensor: &TensorInfo) -> Result<Vec<f32>, String> {
        if !matches!(tensor.dtype, F16 | F32) {
            return Err(format!(
                "GGUF tensor {:?} is not a floating-point weight",
                tensor.name
            ));
        }
        let start = self
            .data_offset
            .checked_add(tensor.offset)
            .ok_or_else(|| format!("GGUF tensor {:?} offset overflows", tensor.name))?;
        let end = start
            .checked_add(tensor.byte_len)
            .ok_or_else(|| format!("GGUF tensor {:?} range overflows", tensor.name))?;
        if end > self.file_len || tensor.byte_len > MAX_TENSOR_BYTES {
            return Err(format!(
                "refusing invalid GGUF range for tensor {:?}",
                tensor.name
            ));
        }
        let mut file = File::open(&self.path).map_err(io_error)?;
        file.seek(SeekFrom::Start(start)).map_err(io_error)?;
        let mut bytes = vec![
            0;
            usize::try_from(tensor.byte_len)
                .map_err(|_| "GGUF tensor does not fit address space")?
        ];
        file.read_exact(&mut bytes).map_err(io_error)?;
        Ok(match tensor.dtype {
            F16 => bytes
                .chunks_exact(2)
                .map(|bits| f16_to_f32(u16::from_le_bytes([bits[0], bits[1]])))
                .collect(),
            F32 => bytes
                .chunks_exact(4)
                .map(|bits| f32::from_le_bytes([bits[0], bits[1], bits[2], bits[3]]))
                .collect(),
            _ => unreachable!(),
        })
    }

    fn validate_birefnet(&self) -> Result<(), String> {
        match self.metadata.get("general.architecture") {
            Some(MetadataValue::String(architecture)) if architecture == "birefnet" => {}
            Some(MetadataValue::String(architecture)) => {
                return Err(format!(
                    "GGUF architecture is {architecture:?}, expected birefnet"
                ));
            }
            _ => return Err("missing GGUF general.architecture".into()),
        }
        let image_size = self.metadata_i64("birefnet.image_size")?;
        let image_multiple = self.metadata_i64("birefnet.image_multiple")?;
        let embed_dim = self.metadata_i64("swin.embed_dim")?;
        if image_size <= 0 || image_multiple <= 0 || image_size % image_multiple != 0 {
            return Err(format!(
                "invalid BiRefNet image metadata size={image_size}, multiple={image_multiple}"
            ));
        }
        if image_size != 1024 || image_multiple != 128 {
            return Err(format!(
                "this native BiRefNet graph supports only image_size=1024 and image_multiple=128, got size={image_size}, multiple={image_multiple}"
            ));
        }
        if !matches!(embed_dim, 96 | 192) {
            return Err(format!("unsupported Swin embed dimension {embed_dim}"));
        }
        match self.metadata.get("birefnet.tensor_data_layout") {
            Some(MetadataValue::String(layout)) if matches!(layout.as_str(), "whcn" | "cwhn") => {}
            Some(MetadataValue::String(layout)) => {
                return Err(format!("unsupported BiRefNet tensor layout {layout:?}"));
            }
            _ => return Err("missing BiRefNet tensor layout metadata".into()),
        }
        // vision.cpp's converter stores patch embedding NHWC even when the
        // model default says NCHW. GGUF writes dimensions in reverse order,
        // therefore Python's [out, height, width, in] is [in, width, height,
        // out] here: RGB's three channels are the first axis.
        let patch = self
            .tensors
            .get("bb.patch_embed.proj.weight")
            .or_else(|| self.tensors.get("bb.patch_embed.projection.weight"))
            .ok_or_else(|| "BiRefNet patch-embedding tensor is absent".to_owned())?;
        if patch.dimensions.len() != 4 || patch.dimensions[0] != 3 {
            return Err(format!(
                "BiRefNet patch embedding must use the converter's NHWC RGB layout; got {:?}",
                patch.dimensions
            ));
        }
        Ok(())
    }
}

fn io_error(error: std::io::Error) -> String {
    error.to_string()
}

fn align_up(value: u64, alignment: u64) -> Result<u64, String> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or_else(|| "GGUF alignment overflows".into())
}

fn read_metadata_value(file: &mut File) -> Result<MetadataValue, String> {
    let kind = read_u32(file)?;
    match kind {
        0 => Ok(MetadataValue::U64(u64::from(read_u8(file)?))),
        1 => Ok(MetadataValue::I64(i64::from(read_u8(file)? as i8))),
        2 => Ok(MetadataValue::U64(u64::from(read_u16(file)?))),
        3 => Ok(MetadataValue::I64(i64::from(read_u16(file)? as i16))),
        4 => Ok(MetadataValue::U64(u64::from(read_u32(file)?))),
        5 => Ok(MetadataValue::I64(i64::from(read_u32(file)? as i32))),
        6 => {
            read_exact(file, 4)?;
            Ok(MetadataValue::String("<f32>".into()))
        }
        7 => Ok(MetadataValue::U64(u64::from(read_u8(file)?))),
        8 => Ok(MetadataValue::String(read_string(file)?)),
        9 => {
            let element_kind = read_u32(file)?;
            let len = read_u64(file)?;
            if len > MAX_ENTRIES {
                return Err("GGUF array has unreasonable length".into());
            }
            for _ in 0..len {
                skip_value(file, element_kind)?;
            }
            Ok(MetadataValue::String("<array>".into()))
        }
        10 => Ok(MetadataValue::U64(read_u64(file)?)),
        11 => Ok(MetadataValue::I64(read_u64(file)? as i64)),
        12 => {
            read_exact(file, 8)?;
            Ok(MetadataValue::String("<f64>".into()))
        }
        _ => Err(format!("unsupported GGUF metadata type {kind}")),
    }
}

fn skip_value(file: &mut File, kind: u32) -> Result<(), String> {
    match kind {
        0 | 1 | 7 => read_exact(file, 1).map(|_| ()),
        2 | 3 => read_exact(file, 2).map(|_| ()),
        4..=6 => read_exact(file, 4).map(|_| ()),
        8 => {
            let _ = read_string(file)?;
            Ok(())
        }
        10..=12 => read_exact(file, 8).map(|_| ()),
        9 => Err("nested GGUF arrays are unsupported".into()),
        _ => Err(format!("unsupported GGUF array element type {kind}")),
    }
}

fn read_string(file: &mut File) -> Result<String, String> {
    let len =
        usize::try_from(read_u64(file)?).map_err(|_| "GGUF string does not fit address space")?;
    if len > MAX_STRING_BYTES {
        return Err(format!(
            "GGUF string exceeds {MAX_STRING_BYTES}-byte safety limit"
        ));
    }
    let bytes = read_exact(file, len)?;
    String::from_utf8(bytes).map_err(|_| "GGUF string is not UTF-8".into())
}
fn read_u8(file: &mut File) -> Result<u8, String> {
    Ok(read_exact(file, 1)?[0])
}
fn read_u16(file: &mut File) -> Result<u16, String> {
    let bytes = read_exact(file, 2)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}
fn read_u32(file: &mut File) -> Result<u32, String> {
    let bytes = read_exact(file, 4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}
fn read_u64(file: &mut File) -> Result<u64, String> {
    let bytes = read_exact(file, 8)?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}
fn read_exact(file: &mut File, len: usize) -> Result<Vec<u8>, String> {
    let mut bytes = vec![0; len];
    file.read_exact(&mut bytes).map_err(io_error)?;
    Ok(bytes)
}

/// IEEE-754 binary16 decode without a runtime dependency on half.
pub(crate) fn f16_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits & 0x8000) << 16;
    let exponent = u32::from((bits >> 10) & 0x1f);
    let fraction = u32::from(bits & 0x03ff);
    let value = if exponent == 0 {
        if fraction == 0 {
            sign
        } else {
            // normalize the subnormal mantissa
            let mut mantissa = fraction;
            let mut exp = 113_u32;
            while mantissa & 0x0400 == 0 {
                mantissa <<= 1;
                exp -= 1;
            }
            sign | (exp << 23) | ((mantissa & 0x03ff) << 13)
        }
    } else if exponent == 31 {
        sign | 0x7f80_0000 | (fraction << 13)
    } else {
        sign | ((exponent + 112) << 23) | (fraction << 13)
    };
    f32::from_bits(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        io::Write,
        sync::atomic::{AtomicUsize, Ordering},
    };

    static FIXTURE_ID: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn f16_values_decode() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xbc00), -1.0);
        assert_eq!(f16_to_f32(0), 0.0);
    }

    #[test]
    fn alignment_rejects_overflow() {
        assert!(align_up(u64::MAX, 32).is_err());
    }

    #[test]
    fn parses_current_birefnet_layout_and_f32_weights() {
        let path = fixture(0);
        let model = GgufModel::open(&path).expect("valid synthetic GGUF");
        assert_eq!(model.metadata_i64("birefnet.image_size").unwrap(), 1024);
        assert_eq!(
            model
                .tensor("bb.patch_embed.proj.weight", &[3, 4, 4, 192])
                .unwrap()
                .dtype,
            F16
        );
        let f32 = model.tensor("f32.bias", &[1]).unwrap();
        assert_eq!(model.tensor_f32(f32).unwrap(), vec![0.5]);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_tensor_data_outside_file() {
        let path = fixture(1 << 20);
        assert!(GgufModel::open(&path).unwrap_err().contains("outside"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_bad_magic_and_nhwc_patch_confusion() {
        let path = fixture(0);
        let mut bytes = fs::read(&path).unwrap();
        bytes[..4].copy_from_slice(b"nope");
        fs::write(&path, bytes).unwrap();
        assert!(GgufModel::open(&path).unwrap_err().contains("not a GGUF"));
        fs::remove_file(path).unwrap();

        let path = fixture_with_patch(&[192, 4, 4, 3]);
        assert!(
            GgufModel::open(&path)
                .unwrap_err()
                .contains("NHWC RGB layout")
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn checks_reference_metadata_when_the_developer_fixture_is_available() {
        let Some(path) = std::env::var_os("DIORAMA_BIREFNET_MODEL").map(PathBuf::from) else {
            return;
        };
        let model = GgufModel::open(&path).expect("reference BiRefNet GGUF parses");
        assert_eq!(model.metadata_i64("birefnet.image_size").unwrap(), 1024);
        assert_eq!(model.metadata_i64("birefnet.image_multiple").unwrap(), 128);
        assert_eq!(model.metadata_i64("swin.embed_dim").unwrap(), 192);
        assert_eq!(
            model
                .tensor("bb.patch_embed.proj.weight", &[3, 4, 4, 192])
                .unwrap()
                .dtype,
            F16
        );
    }

    fn fixture(bad_patch_offset: u64) -> PathBuf {
        fixture_with_patch_and_offset(&[3, 4, 4, 192], bad_patch_offset)
    }
    fn fixture_with_patch(dimensions: &[u64]) -> PathBuf {
        fixture_with_patch_and_offset(dimensions, 0)
    }
    fn fixture_with_patch_and_offset(dimensions: &[u64], patch_offset: u64) -> PathBuf {
        let id = FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "diorama-gguf-test-{}-{id}.gguf",
            std::process::id()
        ));
        let mut data = Vec::new();
        data.extend_from_slice(&MAGIC.to_le_bytes());
        data.extend_from_slice(&VERSION.to_le_bytes());
        data.extend_from_slice(&2_u64.to_le_bytes());
        data.extend_from_slice(&5_u64.to_le_bytes());
        metadata_string(&mut data, "general.architecture", "birefnet");
        metadata_i32(&mut data, "birefnet.image_size", 1024);
        metadata_i32(&mut data, "birefnet.image_multiple", 128);
        metadata_i32(&mut data, "swin.embed_dim", 192);
        metadata_string(&mut data, "birefnet.tensor_data_layout", "whcn");
        tensor_info(
            &mut data,
            "bb.patch_embed.proj.weight",
            dimensions,
            F16,
            patch_offset,
        );
        tensor_info(&mut data, "f32.bias", &[1], F32, 18_432);
        data.resize(align_up(data.len() as u64, 32).unwrap() as usize, 0);
        data.resize(data.len() + 18_436, 0);
        let f32_offset = data.len() - 4;
        data[f32_offset..].copy_from_slice(&0.5_f32.to_le_bytes());
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(&data).unwrap();
        path
    }
    fn string(data: &mut Vec<u8>, value: &str) {
        data.extend_from_slice(&(value.len() as u64).to_le_bytes());
        data.extend_from_slice(value.as_bytes());
    }
    fn metadata_string(data: &mut Vec<u8>, key: &str, value: &str) {
        string(data, key);
        data.extend_from_slice(&8_u32.to_le_bytes());
        string(data, value);
    }
    fn metadata_i32(data: &mut Vec<u8>, key: &str, value: i32) {
        string(data, key);
        data.extend_from_slice(&5_u32.to_le_bytes());
        data.extend_from_slice(&value.to_le_bytes());
    }
    fn tensor_info(data: &mut Vec<u8>, name: &str, dimensions: &[u64], dtype: u32, offset: u64) {
        string(data, name);
        data.extend_from_slice(&(dimensions.len() as u32).to_le_bytes());
        for dim in dimensions {
            data.extend_from_slice(&dim.to_le_bytes());
        }
        data.extend_from_slice(&dtype.to_le_bytes());
        data.extend_from_slice(&offset.to_le_bytes());
    }
}
