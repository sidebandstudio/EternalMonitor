//! Event-driven loopback capture of the default console render endpoint.
//! All COM interfaces and event handles remain on the dedicated audio thread.

use std::time::{Duration, Instant};

use windows::core::GUID;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_FAILED};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use super::pcm::SampleEncoding;
use super::resample::{PcmBlock, PcmFormat};
use super::AudioResult;

const PCM_GUID: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const FLOAT_GUID: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

struct ComApartment(std::marker::PhantomData<std::rc::Rc<()>>);
impl ComApartment {
    fn new() -> windows::core::Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self(std::marker::PhantomData))
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

struct Event(HANDLE);
impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct MixFormat(*mut WAVEFORMATEX);
impl Drop for MixFormat {
    fn drop(&mut self) {
        unsafe {
            CoTaskMemFree(Some(self.0.cast()));
        }
    }
}

fn device_id(device: &IMMDevice) -> windows::core::Result<String> {
    unsafe {
        let id = device.GetId()?;
        let text = id.to_string();
        CoTaskMemFree(Some(id.0.cast()));
        Ok(text?)
    }
}

fn device_name(device: &IMMDevice) -> windows::core::Result<String> {
    unsafe {
        let properties = device.OpenPropertyStore(STGM_READ)?;
        let value = properties.GetValue(&PKEY_Device_FriendlyName)?;
        // PROPVARIANT frees its own storage; the converted PWSTR is separate.
        let name = PropVariantToStringAlloc(&value)?;
        let text = name.to_string();
        CoTaskMemFree(Some(name.0.cast()));
        Ok(text?)
    }
}

fn parse_format(mix: &MixFormat) -> AudioResult<(PcmFormat, SampleEncoding)> {
    if mix.0.is_null() {
        return Err("WASAPI returned no mix format".into());
    }
    let base = unsafe { mix.0.read_unaligned() };
    let (subformat, mask) = if base.wFormatTag == 0xFFFE {
        if base.cbSize < 22 {
            return Err("truncated WASAPI extensible format".into());
        }
        let extended = unsafe { mix.0.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned() };
        (extended.SubFormat, extended.dwChannelMask)
    } else {
        (
            if base.wFormatTag == 3 {
                FLOAT_GUID
            } else if base.wFormatTag == 1 {
                PCM_GUID
            } else {
                return Err("unsupported WASAPI format tag".into());
            },
            0,
        )
    };
    let encoding = match (subformat, base.wBitsPerSample) {
        (guid, 32) if guid == FLOAT_GUID => SampleEncoding::Float32,
        (guid, 16) if guid == PCM_GUID => SampleEncoding::Pcm16,
        (guid, 24) if guid == PCM_GUID => SampleEncoding::Pcm24,
        (guid, 32) if guid == PCM_GUID => SampleEncoding::Pcm32,
        _ => return Err("WASAPI mix must be float32 or PCM16/24/32".into()),
    };
    let format = PcmFormat {
        rate: base.nSamplesPerSec,
        channels: base.nChannels,
        channel_mask: mask,
    };
    if !(8000..=384000).contains(&format.rate)
        || !(1..=8).contains(&format.channels)
        || usize::from(base.nBlockAlign) != usize::from(format.channels) * encoding.bytes()
    {
        return Err("invalid WASAPI mix geometry".into());
    }
    Ok((format, encoding))
}

struct Loopback {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    event: Event,
    id: String,
    name: String,
    format: PcmFormat,
    encoding: SampleEncoding,
    first: bool,
    last_packet: Instant,
}

impl Loopback {
    fn open(enumerator: &IMMDeviceEnumerator) -> AudioResult<Self> {
        unsafe {
            let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let id = device_id(&device)?;
            let name = device_name(&device).unwrap_or_else(|_| "Default PC output".into());
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let mix = MixFormat(client.GetMixFormat()?);
            let (format, encoding) = parse_format(&mix)?;
            // Shared mode needs a zero periodicity. The requested buffer is
            // 10 ms (100 ns units); WASAPI chooses its supported engine period.
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                100_000,
                0,
                mix.0,
                None,
            )?;
            let event = Event(CreateEventW(None, false, false, None)?);
            client.SetEventHandle(event.0)?;
            let capture = client.GetService()?;
            client.Start()?;
            tracing::info!(device = %name, rate = format.rate, channels = format.channels, "WASAPI loopback active");
            Ok(Self {
                capture,
                client,
                event,
                id,
                name,
                format,
                encoding,
                first: true,
                last_packet: Instant::now(),
            })
        }
    }

    fn read(&mut self) -> AudioResult<Option<PcmBlock>> {
        unsafe {
            if self.capture.GetNextPacketSize()? == 0 {
                // Bound the wait so stop, device switches, and a disconnected
                // client are observed without waiting for audio to be played.
                if WaitForSingleObject(self.event.0, 10) == WAIT_FAILED {
                    return Err(windows::core::Error::from_win32().into());
                }
                if self.capture.GetNextPacketSize()? == 0 {
                    if self.last_packet.elapsed() < Duration::from_millis(20) {
                        return Ok(None);
                    }
                    // An idle render engine may publish no buffers at all.
                    // Keep the capture clock moving with digital silence.
                    self.last_packet = Instant::now();
                    return Ok(Some(PcmBlock {
                        format: self.format,
                        samples: vec![
                            0.0;
                            self.format.rate as usize / 50
                                * usize::from(self.format.channels)
                        ],
                        capture_ts_us: crate::clock::host_now_us().saturating_sub(20_000),
                        discontinuity: std::mem::take(&mut self.first),
                    }));
                }
            }
            let mut data = std::ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            let mut qpc_100ns = 0;
            self.capture.GetBuffer(
                &mut data,
                &mut frames,
                &mut flags,
                None,
                Some(&mut qpc_100ns),
            )?;
            if frames == 0 {
                return Ok(None);
            }
            // Always release the WASAPI buffer, including malformed format,
            // non-finite PCM, and oversize backlog error paths.
            let result = (|| -> AudioResult<PcmBlock> {
                if frames > self.format.rate / 10 {
                    return Err("WASAPI capture backlog exceeds 100 ms".into());
                }
                let samples = frames as usize * usize::from(self.format.channels);
                let pcm = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                    vec![0.0; samples]
                } else {
                    if data.is_null() {
                        return Err("WASAPI returned a null PCM buffer".into());
                    }
                    self.encoding.convert(std::slice::from_raw_parts(
                        data,
                        samples * self.encoding.bytes(),
                    ))?
                };
                let mut timestamp = crate::clock::host_now_us()
                    .saturating_sub(u64::from(frames) * 1_000_000 / u64::from(self.format.rate));
                if flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0 {
                    let mut counter = 0;
                    let mut frequency = 0;
                    if QueryPerformanceCounter(&mut counter).is_ok()
                        && QueryPerformanceFrequency(&mut frequency).is_ok()
                        && frequency > 0
                        && counter >= 0
                    {
                        let now_100ns = (counter as u128 * 10_000_000 / frequency as u128) as u64;
                        let age_us = now_100ns.saturating_sub(qpc_100ns) / 10;
                        timestamp = crate::clock::host_now_us().saturating_sub(age_us);
                    }
                }
                Ok(PcmBlock {
                    format: self.format,
                    samples: pcm,
                    capture_ts_us: timestamp,
                    discontinuity: std::mem::take(&mut self.first)
                        || flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0,
                })
            })();
            self.capture.ReleaseBuffer(frames)?;
            self.last_packet = Instant::now();
            result.map(Some)
        }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

pub struct WasapiSource {
    // Declaration order ensures interfaces are released before CoUninitialize.
    stream: Option<Loopback>,
    enumerator: IMMDeviceEnumerator,
    _apartment: ComApartment,
    check_at: Instant,
    retry_at: Instant,
    unavailable_since: Option<Instant>,
}

impl WasapiSource {
    pub fn new() -> AudioResult<Self> {
        let apartment = ComApartment::new()?;
        let enumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let stream = Loopback::open(&enumerator)?;
        Ok(Self {
            stream: Some(stream),
            enumerator,
            _apartment: apartment,
            check_at: Instant::now(),
            retry_at: Instant::now(),
            unavailable_since: None,
        })
    }

    pub fn name(&self) -> &str {
        self.stream
            .as_ref()
            .map_or("Reconnecting PC audio", |stream| stream.name.as_str())
    }

    pub fn read(&mut self) -> AudioResult<Option<PcmBlock>> {
        if self.stream.is_none() {
            if Instant::now() < self.retry_at {
                std::thread::sleep(Duration::from_millis(10));
                return Ok(None);
            }
            match Loopback::open(&self.enumerator) {
                Ok(stream) => {
                    self.stream = Some(stream);
                }
                Err(error) => {
                    if self
                        .unavailable_since
                        .get_or_insert_with(Instant::now)
                        .elapsed()
                        >= Duration::from_secs(5)
                    {
                        return Err(error);
                    }
                    self.retry_at = Instant::now() + Duration::from_millis(250);
                    return Ok(None);
                }
            }
        }
        if Instant::now() >= self.check_at {
            self.check_at = Instant::now() + Duration::from_millis(500);
            let current = unsafe { self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
                .and_then(|device| device_id(&device));
            if current
                .as_ref()
                .is_ok_and(|id| self.stream.as_ref().is_some_and(|stream| &stream.id != id))
                || current.is_err()
            {
                self.reopen();
                return Ok(None);
            }
        }
        match self.stream.as_mut().unwrap().read() {
            Ok(block) => {
                if block.is_some() {
                    self.unavailable_since = None;
                }
                Ok(block)
            }
            Err(error) => {
                if error
                    .downcast_ref::<windows::core::Error>()
                    .is_some_and(|error| {
                        [
                            AUDCLNT_E_DEVICE_INVALIDATED,
                            AUDCLNT_E_RESOURCES_INVALIDATED,
                            AUDCLNT_E_SERVICE_NOT_RUNNING,
                        ]
                        .contains(&error.code())
                    })
                {
                    self.reopen();
                    Ok(None)
                } else {
                    Err(error)
                }
            }
        }
    }

    fn reopen(&mut self) {
        self.stream = None;
        self.unavailable_since.get_or_insert_with(Instant::now);
        self.retry_at = Instant::now();
        tracing::info!("Default PC audio endpoint changed; reopening loopback");
    }
}
