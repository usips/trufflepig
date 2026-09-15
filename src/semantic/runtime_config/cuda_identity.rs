//! CUDA driver UUID enumeration and ordinal resolution.

use anyhow::{Context, Result, bail};
use std::{ffi::CString, ptr::NonNull};

const CUDA_UUID_BYTES: usize = 16;
const MAX_CUDA_DEVICES: usize = 64;

type CuResult = i32;
type CuDevice = i32;
type CuInit = unsafe extern "C" fn(flags: u32) -> CuResult;
type CuDeviceGetCount = unsafe extern "C" fn(count: *mut i32) -> CuResult;
type CuDeviceGet = unsafe extern "C" fn(device: *mut CuDevice, ordinal: i32) -> CuResult;
type CuDeviceGetUuid = unsafe extern "C" fn(uuid: *mut CuUuid, device: CuDevice) -> CuResult;

#[repr(C)]
struct CuUuid {
    bytes: [u8; CUDA_UUID_BYTES],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DriverCudaDevice {
    pub ordinal: u32,
    pub uuid: [u8; CUDA_UUID_BYTES],
}

struct DriverLibrary(NonNull<libc::c_void>);

impl Drop for DriverLibrary {
    fn drop(&mut self) {
        // Every symbol points into this handle, so it must outlive DriverLibrary.
        unsafe {
            libc::dlclose(self.0.as_ptr());
        }
    }
}

struct DriverApi {
    _library: DriverLibrary,
    cu_init: CuInit,
    cu_device_get_count: CuDeviceGetCount,
    cu_device_get: CuDeviceGet,
    cu_device_get_uuid: CuDeviceGetUuid,
}

impl DriverApi {
    fn load() -> Result<Self> {
        let library_name = CString::new("libcuda.so.1").expect("static CUDA library name");
        let handle = NonNull::new(unsafe {
            libc::dlopen(library_name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL)
        })
        .context("cuda_unavailable: libcuda.so.1 is unavailable")?;
        let library = DriverLibrary(handle);

        // The CUDA header aliases cuDeviceGetUuid to the v2 symbol. Resolve
        // the ABI's symbol explicitly so the UUID representation is stable.
        let cu_init = load_symbol(handle, "cuInit")?;
        let cu_device_get_count = load_symbol(handle, "cuDeviceGetCount")?;
        let cu_device_get = load_symbol(handle, "cuDeviceGet")?;
        let cu_device_get_uuid = load_symbol(handle, "cuDeviceGetUuid_v2")?;

        Ok(Self {
            _library: library,
            cu_init,
            cu_device_get_count,
            cu_device_get,
            cu_device_get_uuid,
        })
    }

    fn enumerate(&self) -> Result<Vec<DriverCudaDevice>> {
        cuda_call("cuInit", unsafe { (self.cu_init)(0) })?;

        let mut count = 0_i32;
        cuda_call("cuDeviceGetCount", unsafe {
            (self.cu_device_get_count)(&mut count)
        })?;
        if count < 0 {
            bail!("cuda_unavailable: CUDA driver returned a negative device count");
        }
        let count = usize::try_from(count)
            .context("cuda_unavailable: CUDA device count does not fit usize")?;
        if count > MAX_CUDA_DEVICES {
            bail!(
                "cuda_unavailable: CUDA reported {count} devices, maximum supported is {MAX_CUDA_DEVICES}"
            );
        }

        let mut devices = Vec::with_capacity(count);
        for ordinal in 0..count {
            let ordinal_i32 = i32::try_from(ordinal)
                .context("cuda_unavailable: CUDA device ordinal does not fit i32")?;
            let mut device = 0;
            cuda_call("cuDeviceGet", unsafe {
                (self.cu_device_get)(&mut device, ordinal_i32)
            })?;
            let mut uuid = CuUuid {
                bytes: [0; CUDA_UUID_BYTES],
            };
            cuda_call("cuDeviceGetUuid_v2", unsafe {
                (self.cu_device_get_uuid)(&mut uuid, device)
            })?;
            devices.push(DriverCudaDevice {
                ordinal: u32::try_from(ordinal)
                    .context("cuda_unavailable: CUDA device ordinal overflow")?,
                uuid: uuid.bytes,
            });
        }
        Ok(devices)
    }
}

fn load_symbol<T>(handle: NonNull<libc::c_void>, name: &str) -> Result<T> {
    let name_c = CString::new(name).expect("static CUDA symbol name");
    let symbol = unsafe { libc::dlsym(handle.as_ptr(), name_c.as_ptr()) };
    if symbol.is_null() {
        bail!("cuda_unavailable: CUDA driver symbol {name} is unavailable");
    }
    // CUDA exposes these C functions from the same ABI as dlsym function pointers.
    Ok(unsafe { std::mem::transmute_copy(&symbol) })
}

fn cuda_call(name: &str, status: CuResult) -> Result<()> {
    if status == 0 {
        return Ok(());
    }
    bail!("cuda_unavailable: {name} failed with CUDA status {status}")
}

pub fn resolve_cuda_uuid(wanted: &str) -> Result<u32> {
    let wanted = parse_gpu_uuid(wanted)?;
    let devices = DriverApi::load()?.enumerate()?;
    resolve_uuid_from_devices(wanted, devices)
}

fn resolve_uuid_from_devices(
    wanted: [u8; CUDA_UUID_BYTES],
    devices: impl IntoIterator<Item = DriverCudaDevice>,
) -> Result<u32> {
    devices
        .into_iter()
        .find(|device| device.uuid == wanted)
        .map(|device| device.ordinal)
        .context("cuda_unavailable: configured gpu_uuid is not visible to CUDA")
}

#[cfg(test)]
pub(crate) fn resolve_cuda_uuid_with<F>(wanted: &str, enumerate: F) -> Result<u32>
where
    F: FnOnce() -> Result<Vec<DriverCudaDevice>>,
{
    let wanted = parse_gpu_uuid(wanted)?;
    resolve_uuid_from_devices(wanted, enumerate()?)
}

fn parse_gpu_uuid(input: &str) -> Result<[u8; CUDA_UUID_BYTES]> {
    let prefix = input
        .get(..4)
        .filter(|prefix| prefix.eq_ignore_ascii_case("GPU-"))
        .context("cuda_unavailable: configured gpu_uuid must be a canonical GPU UUID")?;
    let _ = prefix;
    let encoded = input
        .get(4..)
        .context("cuda_unavailable: configured gpu_uuid must be a canonical GPU UUID")?;
    let group_lengths = [8, 4, 4, 4, 12];
    let mut offset = 0;
    let mut output = [0_u8; CUDA_UUID_BYTES];
    let mut output_offset = 0;
    for (group_index, &group_length) in group_lengths.iter().enumerate() {
        let group = encoded
            .get(offset..offset + group_length)
            .context("cuda_unavailable: configured gpu_uuid must be a canonical GPU UUID")?;
        for pair in group.as_bytes().chunks_exact(2) {
            let high = hex_nibble(pair[0])
                .context("cuda_unavailable: configured gpu_uuid must be a canonical GPU UUID")?;
            let low = hex_nibble(pair[1])
                .context("cuda_unavailable: configured gpu_uuid must be a canonical GPU UUID")?;
            output[output_offset] = (high << 4) | low;
            output_offset += 1;
        }
        offset += group_length;
        if group_index + 1 != group_lengths.len() {
            if encoded.as_bytes().get(offset) != Some(&b'-') {
                bail!("cuda_unavailable: configured gpu_uuid must be a canonical GPU UUID");
            }
            offset += 1;
        }
    }
    if offset != encoded.len() {
        bail!("cuda_unavailable: configured gpu_uuid must be a canonical GPU UUID");
    }
    Ok(output)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: &str = "GPU-00112233-4455-6677-8899-aabbccddeeff";
    const OTHER: &str = "GPU-ffeeddcc-bbaa-9988-7766-554433221100";

    fn fake_device(uuid: &str, ordinal: u32) -> DriverCudaDevice {
        DriverCudaDevice {
            ordinal,
            uuid: parse_gpu_uuid(uuid).unwrap(),
        }
    }

    #[test]
    fn resolves_uuid_after_cuda_order_permutation() {
        let ordinal = resolve_cuda_uuid_with(TARGET, || {
            Ok(vec![fake_device(OTHER, 0), fake_device(TARGET, 1)])
        })
        .unwrap();
        assert_eq!(ordinal, 1);
    }

    #[test]
    fn rejects_hidden_uuid_and_empty_visible_device_list() {
        assert!(resolve_cuda_uuid_with(TARGET, || Ok(vec![fake_device(OTHER, 0)])).is_err());
        assert!(resolve_cuda_uuid_with(TARGET, || Ok(Vec::new())).is_err());
    }

    #[test]
    fn rejects_malformed_uuid_before_enumerating() {
        let called = std::cell::Cell::new(false);
        let result = resolve_cuda_uuid_with("GPU-not-a-uuid", || {
            called.set(true);
            Ok(vec![fake_device(TARGET, 0)])
        });
        assert!(result.is_err());
        assert!(!called.get());
    }
}
