// Micro Music Room v2. Copyright (C) 2026 Marko Idžan. Licensed under GPL-3.0-or-later.
// Rewritten in Rust from Micro Music Room (WPF) 1.0.3. Modified November 24, 2026.

//! Windows Media Foundation fallback decoder, for formats Symphonia cannot open (WMA).
//!
//! It uses the Media Foundation *Source Reader* to pull decoded PCM and presents it as a rodio
//! [`Source`]. Windows ships the WMA decoders, so nothing extra is bundled, the same as v1.
//!
//! This module only compiles on Windows.

use rodio::{ChannelCount, SampleRate, Source, source::SeekError};
use std::{
    cell::Cell,
    io,
    num::NonZero,
    os::windows::ffi::OsStrExt,
    path::Path,
    ptr, slice,
    sync::{Arc, OnceLock},
    time::Duration,
};
use windows::{
    Win32::{
        Media::MediaFoundation::{
            IMFAttributes, IMFSample, IMFSourceReader, MF_MT_AUDIO_BITS_PER_SAMPLE,
            MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE,
            MF_MT_SUBTYPE, MF_PD_DURATION, MF_SOURCE_READER_ALL_STREAMS,
            MF_SOURCE_READER_FIRST_AUDIO_STREAM, MF_SOURCE_READER_MEDIASOURCE,
            MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR, MF_VERSION,
            MFAudioFormat_Float, MFAudioFormat_PCM, MFCreateMediaType, MFCreateSourceReaderFromURL,
            MFMediaType_Audio, MFSTARTUP_FULL, MFStartup,
        },
        System::{
            Com::{
                COINIT_MULTITHREADED, CoInitializeEx,
                StructuredStorage::{PROPVARIANT, PropVariantToUInt64},
            },
            Variant::VT_I8,
        },
    },
    core::{Error, GUID, HRESULT, HSTRING},
};

/// Source Reader stream indexes are `u32`, the header constants are negative `i32`s.
const AUDIO_STREAM: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
const ALL_STREAMS: u32 = MF_SOURCE_READER_ALL_STREAMS.0 as u32;
const MEDIA_SOURCE: u32 = MF_SOURCE_READER_MEDIASOURCE.0 as u32;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Float32,
    Pcm16,
}

pub struct WmfSource {
    reader: IMFSourceReader,
    format: Format,
    channels: ChannelCount,
    rate: SampleRate,
    duration: Option<Duration>,
    buf: Vec<f32>,
    pos: usize,
    ended: bool,
}

// SAFETY: `IMFSourceReader` is implemented by free-threaded (MTA) Media Foundation objects, and a
// `WmfSource` is only ever used by one thread at a time (rodio moves the source to its audio
// thread and never shares it). Moving it between threads is therefore sound.
unsafe impl Send for WmfSource {}

impl WmfSource {
    pub fn open(path: &Path) -> windows::core::Result<Self> {
        startup()?;
        // Wide-encode directly from the OS string so non-ASCII paths (č, ć, š, đ, ž) survive.
        let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        let url = HSTRING::from_wide(&wide);

        // SAFETY: plain Media Foundation calls with valid, live arguments.
        unsafe {
            let reader = MFCreateSourceReaderFromURL(&url, None::<&IMFAttributes>)?;
            reader.SetStreamSelection(ALL_STREAMS, false)?;
            reader.SetStreamSelection(AUDIO_STREAM, true)?;

            // Ask for 32-bit float; if the pipeline cannot supply it, fall back to 16-bit PCM.
            let format = match configure(&reader, Format::Float32) {
                Ok(()) => Format::Float32,
                Err(_) => {
                    configure(&reader, Format::Pcm16)?;
                    Format::Pcm16
                }
            };

            let actual = reader.GetCurrentMediaType(AUDIO_STREAM)?;
            let channels = actual.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)?;
            let rate = actual.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?;
            let invalid = || Error::from(HRESULT(0x8000_4005_u32 as i32)); // E_FAIL
            let channels = u16::try_from(channels)
                .ok()
                .and_then(NonZero::new)
                .ok_or_else(invalid)?;
            let rate = NonZero::new(rate).ok_or_else(invalid)?;

            // Duration is in 100 ns units. Not every file reports it.
            let duration = reader
                .GetPresentationAttribute(MEDIA_SOURCE, &MF_PD_DURATION)
                .ok()
                .and_then(|v| PropVariantToUInt64(&v).ok())
                .map(|hns| Duration::from_nanos(hns.saturating_mul(100)));

            Ok(Self {
                reader,
                format,
                channels,
                rate,
                duration,
                buf: Vec::new(),
                pos: 0,
                ended: false,
            })
        }
    }

    /// Pull decoded samples until the buffer has data or the stream ends.
    fn refill(&mut self) {
        self.buf.clear();
        self.pos = 0;
        while self.buf.is_empty() && !self.ended {
            let mut flags = 0u32;
            let mut sample: Option<IMFSample> = None;
            // SAFETY: out-pointers are valid for the duration of the call.
            let read = unsafe {
                self.reader.ReadSample(
                    AUDIO_STREAM,
                    0,
                    None,
                    Some(&mut flags),
                    None,
                    Some(&mut sample),
                )
            };
            if read.is_err() || flags & (MF_SOURCE_READERF_ERROR.0 as u32) != 0 {
                self.ended = true;
                break;
            }
            if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
                self.ended = true;
            }
            if let Some(sample) = sample {
                if self.append(&sample).is_err() {
                    self.ended = true;
                }
            }
        }
    }

    fn append(&mut self, sample: &IMFSample) -> windows::core::Result<()> {
        // SAFETY: the locked pointer is valid for `len` bytes until `Unlock`, and we copy out of it
        // before unlocking.
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data: *mut u8 = ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut data, None, Some(&mut len))?;
            if !data.is_null() {
                let bytes = slice::from_raw_parts(data, len as usize);
                match self.format {
                    Format::Float32 => self.buf.extend(
                        bytes
                            .chunks_exact(4)
                            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                    ),
                    Format::Pcm16 => self.buf.extend(
                        bytes
                            .chunks_exact(2)
                            .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0),
                    ),
                }
            }
            buffer.Unlock()
        }
    }
}

impl Iterator for WmfSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.pos >= self.buf.len() {
            self.refill();
        }
        let sample = self.buf.get(self.pos).copied()?;
        self.pos += 1;
        Some(sample)
    }
}

impl Source for WmfSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        self.channels
    }

    fn sample_rate(&self) -> SampleRate {
        self.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.duration
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        let hns = i64::try_from(pos.as_nanos() / 100).unwrap_or(i64::MAX);
        let mut position = PROPVARIANT::default();
        // SAFETY: building a VT_I8 PROPVARIANT by hand (windows-rs has no helper for a plain
        // integer); it owns no heap data, so it needs no clearing.
        let result = unsafe {
            let inner = &mut *position.Anonymous.Anonymous;
            inner.vt = VT_I8;
            inner.Anonymous.hVal = hns;
            self.reader.SetCurrentPosition(&GUID::zeroed(), &position)
        };
        match result {
            Ok(()) => {
                self.buf.clear();
                self.pos = 0;
                self.ended = false;
                Ok(())
            }
            Err(e) => Err(SeekError::Other(Arc::new(io::Error::other(e.message())))),
        }
    }
}

/// Set the reader's output to the given PCM flavour.
///
/// # Safety
/// `reader` must be a valid, live source reader.
unsafe fn configure(reader: &IMFSourceReader, format: Format) -> windows::core::Result<()> {
    unsafe {
        let wanted = MFCreateMediaType()?;
        wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        match format {
            Format::Float32 => {
                wanted.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
                wanted.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 32)?;
            }
            Format::Pcm16 => {
                wanted.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
                wanted.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            }
        }
        reader.SetCurrentMediaType(AUDIO_STREAM, None, &wanted)
    }
}

thread_local! {
    static COM_READY: Cell<bool> = const { Cell::new(false) };
}

/// Initialise COM (once per calling thread) and Media Foundation (once per process).
///
/// COM is initialised multithreaded; if the thread already runs as a single-threaded apartment
/// (such as a UI thread) the call reports a mode conflict, which is fine and ignored.
fn startup() -> windows::core::Result<()> {
    COM_READY.with(|ready| {
        if !ready.get() {
            // SAFETY: plain COM initialisation. The result is deliberately ignored.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            ready.set(true);
        }
    });
    static MF: OnceLock<Result<(), HRESULT>> = OnceLock::new();
    // Never shut down: Media Foundation lives for the whole process.
    MF.get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| e.code()))
        .map_err(Error::from)
}
