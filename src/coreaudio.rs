//! Minimal hand-written bindings to the Core Audio HAL (AudioObject / AudioDevice IOProc).

#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use eyre::{bail, eyre, Result};
use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr;

pub type AudioObjectID = u32;
pub type OSStatus = i32;
type CFStringRef = *const c_void;
type AudioDeviceIOProcID = *mut c_void;

const fn fcc(s: &[u8; 4]) -> u32 {
    ((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | (s[3] as u32)
}

pub const kAudioObjectSystemObject: AudioObjectID = 1;
pub const kAudioObjectUnknown: AudioObjectID = 0;

const kScopeGlobal: u32 = fcc(b"glob");
const kScopeInput: u32 = fcc(b"inpt");
const kScopeOutput: u32 = fcc(b"outp");
const kElementMain: u32 = 0;

const kAudioHardwarePropertyDevices: u32 = fcc(b"dev#");
const kAudioHardwarePropertyDefaultInputDevice: u32 = fcc(b"dIn ");
const kAudioHardwarePropertyTranslateUIDToDevice: u32 = fcc(b"uidd");
const kAudioObjectPropertyName: u32 = fcc(b"lnam");
const kAudioDevicePropertyDeviceUID: u32 = fcc(b"uid ");
const kAudioDevicePropertyStreamConfiguration: u32 = fcc(b"slay");
const kAudioDevicePropertyNominalSampleRate: u32 = fcc(b"nsrt");
const kAudioDevicePropertyStreamFormat: u32 = fcc(b"sfmt");
const kAudioDevicePropertyBufferFrameSize: u32 = fcc(b"fsiz");
const kAudioDevicePropertyTransportType: u32 = fcc(b"tran");
const kAudioDeviceTransportTypeBuiltIn: u32 = fcc(b"bltn");

const kAudioFormatLinearPCM: u32 = fcc(b"lpcm");
const kAudioFormatFlagIsFloat: u32 = 1 << 0;
const kAudioFormatFlagIsNonInterleaved: u32 = 1 << 5;

const kCFStringEncodingUTF8: u32 = 0x0800_0100;

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioObjectPropertyAddress {
    mSelector: u32,
    mScope: u32,
    mElement: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct AudioStreamBasicDescription {
    pub mSampleRate: f64,
    pub mFormatID: u32,
    pub mFormatFlags: u32,
    pub mBytesPerPacket: u32,
    pub mFramesPerPacket: u32,
    pub mBytesPerFrame: u32,
    pub mChannelsPerFrame: u32,
    pub mBitsPerChannel: u32,
    pub mReserved: u32,
}

#[repr(C)]
pub struct AudioBuffer {
    pub mNumberChannels: u32,
    pub mDataByteSize: u32,
    pub mData: *mut c_void,
}

#[repr(C)]
pub struct AudioBufferList {
    pub mNumberBuffers: u32,
    pub mBuffers: [AudioBuffer; 1], // variable length
}

impl AudioBufferList {
    /// The buffers of a list allocated by Core Audio.
    ///
    /// # Safety
    /// `list` must point to a valid AudioBufferList with `mNumberBuffers` entries.
    pub unsafe fn buffers<'a>(list: *const AudioBufferList) -> &'a [AudioBuffer] {
        if list.is_null() {
            return &[];
        }
        std::slice::from_raw_parts((*list).mBuffers.as_ptr(), (*list).mNumberBuffers as usize)
    }
}

pub type AudioDeviceIOProc = extern "C" fn(
    AudioObjectID,
    *const c_void, // AudioTimeStamp* now
    *const AudioBufferList,
    *const c_void, // input time
    *mut AudioBufferList,
    *const c_void, // output time
    *mut c_void,
) -> OSStatus;

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyDataSize(
        obj: AudioObjectID,
        addr: *const AudioObjectPropertyAddress,
        qual_size: u32,
        qual: *const c_void,
        out_size: *mut u32,
    ) -> OSStatus;
    fn AudioObjectGetPropertyData(
        obj: AudioObjectID,
        addr: *const AudioObjectPropertyAddress,
        qual_size: u32,
        qual: *const c_void,
        io_size: *mut u32,
        out: *mut c_void,
    ) -> OSStatus;
    fn AudioObjectSetPropertyData(
        obj: AudioObjectID,
        addr: *const AudioObjectPropertyAddress,
        qual_size: u32,
        qual: *const c_void,
        size: u32,
        data: *const c_void,
    ) -> OSStatus;
    fn AudioDeviceCreateIOProcID(
        dev: AudioObjectID,
        proc_: AudioDeviceIOProc,
        client: *mut c_void,
        out_id: *mut AudioDeviceIOProcID,
    ) -> OSStatus;
    fn AudioDeviceDestroyIOProcID(dev: AudioObjectID, id: AudioDeviceIOProcID) -> OSStatus;
    fn AudioDeviceStart(dev: AudioObjectID, id: AudioDeviceIOProcID) -> OSStatus;
    fn AudioDeviceStop(dev: AudioObjectID, id: AudioDeviceIOProcID) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, enc: u32) -> CFStringRef;
    fn CFStringGetCString(s: CFStringRef, buf: *mut c_char, len: isize, enc: u32) -> u8;
    fn CFRelease(obj: *const c_void);
}

fn check(status: OSStatus, what: &str) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        let b = status.to_be_bytes();
        let code = if b.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
            format!("'{}'", String::from_utf8_lossy(&b))
        } else {
            status.to_string()
        };
        Err(eyre!("{what} failed: OSStatus {code}"))
    }
}

fn addr(sel: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: sel,
        mScope: scope,
        mElement: kElementMain,
    }
}

fn get<T: Copy + Default>(obj: AudioObjectID, a: AudioObjectPropertyAddress) -> Result<T> {
    let mut v = T::default();
    let mut size = std::mem::size_of::<T>() as u32;
    let st = unsafe {
        AudioObjectGetPropertyData(
            obj,
            &a,
            0,
            ptr::null(),
            &mut size,
            &mut v as *mut T as *mut c_void,
        )
    };
    check(st, "AudioObjectGetPropertyData")?;
    Ok(v)
}

/// Raw property bytes (8-byte aligned storage) and their length.
fn get_bytes(obj: AudioObjectID, a: AudioObjectPropertyAddress) -> Result<(Vec<u64>, usize)> {
    let mut size = 0u32;
    check(
        unsafe { AudioObjectGetPropertyDataSize(obj, &a, 0, ptr::null(), &mut size) },
        "AudioObjectGetPropertyDataSize",
    )?;
    // u64 storage keeps the buffer 8-byte aligned for structs like AudioBufferList.
    let mut buf = vec![0u64; (size as usize).div_ceil(8).max(1)];
    check(
        unsafe {
            AudioObjectGetPropertyData(
                obj,
                &a,
                0,
                ptr::null(),
                &mut size,
                buf.as_mut_ptr() as *mut c_void,
            )
        },
        "AudioObjectGetPropertyData",
    )?;
    Ok((buf, size as usize))
}

fn get_string(obj: AudioObjectID, a: AudioObjectPropertyAddress) -> Result<String> {
    let s: usize = get::<usize>(obj, a)?;
    let s = s as CFStringRef;
    if s.is_null() {
        return Ok(String::new());
    }
    let mut buf = vec![0 as c_char; 1024];
    let ok = unsafe {
        CFStringGetCString(
            s,
            buf.as_mut_ptr(),
            buf.len() as isize,
            kCFStringEncodingUTF8,
        )
    };
    unsafe { CFRelease(s) };
    if ok == 0 {
        bail!("CFStringGetCString failed");
    }
    Ok(unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned())
}

#[derive(Clone, Debug)]
pub struct Device {
    pub id: AudioObjectID,
    pub name: String,
    pub uid: String,
    pub input_channels: u32,
    pub output_channels: u32,
}

fn channels(id: AudioObjectID, scope: u32) -> u32 {
    let Ok((buf, _)) = get_bytes(id, addr(kAudioDevicePropertyStreamConfiguration, scope)) else {
        return 0;
    };
    let list = buf.as_ptr() as *const AudioBufferList;
    unsafe { AudioBufferList::buffers(list) }
        .iter()
        .map(|b| b.mNumberChannels)
        .sum()
}

impl Device {
    pub fn from_id(id: AudioObjectID) -> Result<Self> {
        Ok(Self {
            id,
            name: get_string(id, addr(kAudioObjectPropertyName, kScopeGlobal)).unwrap_or_default(),
            uid: get_string(id, addr(kAudioDevicePropertyDeviceUID, kScopeGlobal))
                .unwrap_or_default(),
            input_channels: channels(id, kScopeInput),
            output_channels: channels(id, kScopeOutput),
        })
    }

    pub fn all() -> Result<Vec<Self>> {
        let (buf, size) = get_bytes(
            kAudioObjectSystemObject,
            addr(kAudioHardwarePropertyDevices, kScopeGlobal),
        )?;
        let ids = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u32, size / 4) };
        Ok(ids
            .iter()
            .filter_map(|&id| Self::from_id(id).ok())
            .collect())
    }

    pub fn default_input() -> Result<Self> {
        let id: u32 = get(
            kAudioObjectSystemObject,
            addr(kAudioHardwarePropertyDefaultInputDevice, kScopeGlobal),
        )?;
        Self::from_id(id)
    }

    pub fn by_uid(uid: &str) -> Result<Option<Self>> {
        let c = CString::new(uid)?;
        let s =
            unsafe { CFStringCreateWithCString(ptr::null(), c.as_ptr(), kCFStringEncodingUTF8) };
        if s.is_null() {
            bail!("CFStringCreateWithCString failed");
        }
        let a = addr(kAudioHardwarePropertyTranslateUIDToDevice, kScopeGlobal);
        let mut id: AudioObjectID = 0;
        let mut size = 4u32;
        let st = unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject,
                &a,
                std::mem::size_of::<CFStringRef>() as u32,
                &s as *const CFStringRef as *const c_void,
                &mut size,
                &mut id as *mut u32 as *mut c_void,
            )
        };
        unsafe { CFRelease(s) };
        check(st, "TranslateUIDToDevice")?;
        if id == kAudioObjectUnknown {
            return Ok(None);
        }
        Self::from_id(id).map(Some)
    }

    pub fn is_builtin(&self) -> bool {
        get::<u32>(
            self.id,
            addr(kAudioDevicePropertyTransportType, kScopeGlobal),
        )
        .map(|t| t == kAudioDeviceTransportTypeBuiltIn)
        .unwrap_or(false)
    }

    pub fn sample_rate(&self) -> Result<f64> {
        get(
            self.id,
            addr(kAudioDevicePropertyNominalSampleRate, kScopeGlobal),
        )
    }

    /// The IOProc-side format of the device's streams in one direction.
    pub fn format(&self, input: bool) -> Result<AudioStreamBasicDescription> {
        get(
            self.id,
            addr(
                kAudioDevicePropertyStreamFormat,
                if input { kScopeInput } else { kScopeOutput },
            ),
        )
    }

    pub fn set_buffer_frames(&self, frames: u32) -> Result<()> {
        let a = addr(kAudioDevicePropertyBufferFrameSize, kScopeGlobal);
        check(
            unsafe {
                AudioObjectSetPropertyData(
                    self.id,
                    &a,
                    0,
                    ptr::null(),
                    4,
                    &frames as *const u32 as *const c_void,
                )
            },
            "set BufferFrameSize",
        )
    }

    pub fn buffer_frames(&self) -> Result<u32> {
        get(
            self.id,
            addr(kAudioDevicePropertyBufferFrameSize, kScopeGlobal),
        )
    }
}

/// Fails unless the stream format is 32-bit float PCM, which is what the HAL hands IOProcs.
pub fn ensure_float(f: &AudioStreamBasicDescription, what: &str) -> Result<()> {
    if f.mFormatID != kAudioFormatLinearPCM
        || f.mFormatFlags & kAudioFormatFlagIsFloat == 0
        || f.mBitsPerChannel != 32
    {
        bail!("{what}: unsupported stream format {f:?} (expected Float32 PCM)");
    }
    Ok(())
}

/// A running IOProc on a device; stops and is destroyed on drop.
pub struct IoProc {
    device: AudioObjectID,
    id: AudioDeviceIOProcID,
    _ctx: Box<dyn std::any::Any>,
}

unsafe impl Send for IoProc {}

impl IoProc {
    /// Installs `cb` as the IOProc of `device` and starts it.
    ///
    /// `cb(input, output)` runs on Core Audio's real-time thread; it must not block or allocate.
    pub fn start<F>(device: AudioObjectID, cb: F) -> Result<Self>
    where
        F: FnMut(&[AudioBuffer], &mut [AudioBuffer]) + Send + 'static,
    {
        extern "C" fn trampoline<F: FnMut(&[AudioBuffer], &mut [AudioBuffer])>(
            _dev: AudioObjectID,
            _now: *const c_void,
            input: *const AudioBufferList,
            _in_time: *const c_void,
            output: *mut AudioBufferList,
            _out_time: *const c_void,
            ctx: *mut c_void,
        ) -> OSStatus {
            let f = unsafe { &mut *(ctx as *mut F) };
            let inp = unsafe { AudioBufferList::buffers(input) };
            let out: &mut [AudioBuffer] = if output.is_null() {
                &mut []
            } else {
                unsafe {
                    std::slice::from_raw_parts_mut(
                        (*output).mBuffers.as_mut_ptr(),
                        (*output).mNumberBuffers as usize,
                    )
                }
            };
            f(inp, out);
            0
        }

        let mut boxed: Box<F> = Box::new(cb);
        let ctx = &mut *boxed as *mut F as *mut c_void;
        let mut id: AudioDeviceIOProcID = ptr::null_mut();
        check(
            unsafe { AudioDeviceCreateIOProcID(device, trampoline::<F>, ctx, &mut id) },
            "AudioDeviceCreateIOProcID",
        )?;
        if let Err(e) = check(unsafe { AudioDeviceStart(device, id) }, "AudioDeviceStart") {
            unsafe { AudioDeviceDestroyIOProcID(device, id) };
            return Err(e);
        }
        Ok(Self {
            device,
            id,
            _ctx: boxed,
        })
    }
}

impl Drop for IoProc {
    fn drop(&mut self) {
        unsafe {
            AudioDeviceStop(self.device, self.id);
            AudioDeviceDestroyIOProcID(self.device, self.id);
        }
    }
}

pub fn is_non_interleaved(f: &AudioStreamBasicDescription) -> bool {
    f.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0
}
