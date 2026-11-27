//! Reproducción de audio y vídeo, escrita desde cero para Xool Files.
//! Audio: rodio + symphonia. Vídeo: Media Foundation (frames BGRA).

#![allow(unsafe_op_in_unsafe_fn)]

use crate::i18n::tr;
use std::collections::VecDeque;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::RenderImage;
use rodio::buffer::SamplesBuffer;
use rodio::{Decoder, MixerDeviceSink, Player};
use symphonia::core::audio::{AudioBuffer, Signal};
use symphonia::core::codecs::{CODEC_TYPE_NULL, Decoder as SymphoniaDecoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;
use windows::core::{GUID, PCWSTR};
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSample, MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READER_MEDIASOURCE, MF_MT_DEFAULT_STRIDE,
    MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_MT_TRANSFER_FUNCTION,
    MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX, MF_PD_DURATION, MF_VERSION, MFMediaType_Video,
    MFNominalRange_0_255, MFSTARTUP_LITE, MFVideoFormat_NV12, MFVideoTransferMatrix_BT2020_10,
    MFVideoTransferMatrix_BT601, MFVideoTransferMatrix_BT709, MFVideoTransFunc_2020,
    MFVideoTransFunc_2020_const, MFVideoTransFunc_2084, MFVideoTransFunc_22, MFVideoTransFunc_240M,
    MFVideoTransFunc_709, MFVideoTransFunc_709_sym, MFVideoTransFunc_HLG, MFCreateAttributes,
    MFCreateMediaType, MFCreateSourceReaderFromURL, MFShutdown, MFStartup,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Variant::VT_I8;

use crate::platform;

// ---------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------

pub struct AudioTrack {
    pub waveform: Vec<f32>,
    pub duration: f32,
}

/// Analiza un archivo de audio y devuelve su forma de onda (120 barras)
/// y su duración. Se ejecuta fuera del hilo de interfaz.
pub fn analyze_audio(path: &Path) -> Option<AudioTrack> {
    let file = File::open(path).ok()?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(extension) = path.extension() {
        hint.with_extension(&extension.to_string_lossy());
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)?;
    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44_100).max(1);
    let declared_frames = track.codec_params.n_frames;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .ok()?;

    let bucket_frames = (sample_rate as u64 / 100).max(1);
    let mut peaks: Vec<f32> = Vec::new();
    let mut bucket_peak = 0.0f32;
    let mut bucket_fill = 0u64;
    let mut total_frames = 0u64;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(_) => continue,
        };

        let mut converted =
            AudioBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
        decoded.convert(&mut converted);
        let frames = converted.frames();
        let channels = converted.spec().channels.count();

        for frame in 0..frames {
            let mut peak = 0.0f32;
            for channel in 0..channels {
                if let Some(sample) = converted.chan(channel).get(frame) {
                    peak = peak.max(sample.abs());
                }
            }
            bucket_peak = bucket_peak.max(peak);
            bucket_fill += 1;
            if bucket_fill >= bucket_frames {
                peaks.push(bucket_peak);
                bucket_peak = 0.0;
                bucket_fill = 0;
            }
        }
        total_frames += frames as u64;
    }

    if bucket_fill > 0 {
        peaks.push(bucket_peak);
    }
    if peaks.is_empty() {
        return None;
    }

    const BARS: usize = 120;
    let mut waveform = vec![0.0f32; BARS];
    for (index, bar) in waveform.iter_mut().enumerate() {
        let start = index * peaks.len() / BARS;
        let end = (((index + 1) * peaks.len()) / BARS)
            .max(start + 1)
            .min(peaks.len());
        *bar = peaks[start..end]
            .iter()
            .copied()
            .fold(0.0f32, f32::max);
    }

    let loudest = waveform.iter().copied().fold(0.0f32, f32::max).max(0.0001);
    for bar in &mut waveform {
        *bar = (*bar / loudest).powf(0.65).clamp(0.05, 1.0);
    }

    let duration = match declared_frames {
        Some(frames) if frames > 0 => frames as f32 / sample_rate as f32,
        _ => total_frames as f32 / sample_rate as f32,
    };

    Some(AudioTrack {
        waveform,
        duration,
    })
}

struct AudioEngine {
    player: Arc<Player>,
    seek_target: Arc<Mutex<Option<Duration>>>,
    wake_seek: mpsc::SyncSender<()>,
}

impl AudioEngine {
    fn new(player: Player, sink: MixerDeviceSink) -> Self {
        let player = Arc::new(player);
        let seek_target = Arc::new(Mutex::new(None));
        let (wake_seek, requests) = mpsc::sync_channel(1);
        let worker_player = player.clone();
        let worker_target = seek_target.clone();
        std::thread::spawn(move || {
            let _sink = sink;
            while requests.recv().is_ok() {
                let target = worker_target.lock().ok().and_then(|mut latest| latest.take());
                if let Some(target) = target {
                    let _ = worker_player.try_seek(target);
                }
            }
        });
        Self { player, seek_target, wake_seek }
    }

    fn seek(&self, position: f32) {
        if let Ok(mut latest) = self.seek_target.lock() {
            *latest = Some(Duration::from_secs_f32(position));
        }
        let _ = self.wake_seek.try_send(());
    }
}

/// Reproductor de audio con posición propia (independiente del sink).
pub struct AudioPlayback {
    pub path: PathBuf,
    pub waveform: Vec<f32>,
    pub duration: f32,
    pub position: f32,
    pub playing: bool,
    pub volume: f32,
    pub loading: bool,
    pub error: Option<String>,
    engine: Option<AudioEngine>,
    base_position: f32,
    started_at: Option<Instant>,
}

impl AudioPlayback {
    pub fn new(path: PathBuf, volume: f32) -> Self {
        Self {
            path,
            waveform: Vec::new(),
            duration: 0.0,
            position: 0.0,
            playing: false,
            volume,
            loading: true,
            error: None,
            engine: None,
            base_position: 0.0,
            started_at: None,
        }
    }

    pub fn set_track(&mut self, track: AudioTrack) {
        self.waveform = track.waveform;
        self.duration = track.duration;
        self.loading = false;
    }

    pub fn toggle(&mut self) {
        if self.playing {
            self.pause();
        } else {
            self.play();
        }
    }

    pub fn play(&mut self) {
        self.error = None;
        if self.duration > 0.0 && self.position >= self.duration - 0.05 {
            self.position = 0.0;
            self.stop_engine();
        }

        if self.engine.is_none() {
            match open_audio_engine(&self.path, self.volume) {
                Ok((sink, player)) => {
                    let engine = AudioEngine::new(player, sink);
                    if self.position > 0.05 { engine.seek(self.position); }
                    engine.player.play();
                    self.engine = Some(engine);
                }
                Err(error) => {
                    self.error = Some(error);
                    self.playing = false;
                    return;
                }
            }
        } else if let Some(engine) = &self.engine {
            engine.player.play();
        }

        self.base_position = self.position;
        self.started_at = Some(Instant::now());
        self.playing = true;
    }

    pub fn pause(&mut self) {
        if let Some(engine) = &self.engine {
            engine.player.pause();
        }
        if let Some(started) = self.started_at.take() {
            self.position = (self.base_position + started.elapsed().as_secs_f32())
                .min(self.duration.max(0.0));
        }
        self.base_position = self.position;
        self.playing = false;
    }

    pub fn seek_ratio(&mut self, ratio: f32) {
        if self.duration <= 0.0 {
            return;
        }
        let target = (ratio.clamp(0.0, 1.0) * self.duration).clamp(0.0, self.duration);
        self.position = target;
        self.base_position = target;
        self.started_at = if self.playing { Some(Instant::now()) } else { None };
        if let Some(engine) = &self.engine {
            engine.seek(target);
        }
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        if let Some(engine) = &self.engine {
            engine.player.set_volume(self.volume);
        }
    }

    /// Avanza el reloj interno. Devuelve true si la posición cambió.
    pub fn tick(&mut self) -> bool {
        if !self.playing {
            return false;
        }
        let Some(started) = self.started_at else {
            return false;
        };
        let next = (self.base_position + started.elapsed().as_secs_f32())
            .min(self.duration.max(0.0));
        let changed = (next - self.position).abs() > 0.01;
        self.position = next;

        let ended = self
            .engine
            .as_ref()
            .map(|engine| engine.player.empty())
            .unwrap_or(true);
        if ended || (self.duration > 0.0 && self.position >= self.duration - 0.02) {
            self.playing = false;
            self.position = self.duration;
            self.base_position = self.duration;
            self.started_at = None;
            self.stop_engine();
        }
        changed
    }

    pub fn stop(&mut self) {
        self.playing = false;
        self.position = 0.0;
        self.base_position = 0.0;
        self.started_at = None;
        self.stop_engine();
    }

    fn stop_engine(&mut self) {
        if let Some(engine) = &self.engine {
            engine.player.pause();
        }
        self.engine = None;
    }
}

impl Drop for AudioPlayback {
    fn drop(&mut self) {
        self.stop_engine();
    }
}

fn open_audio_engine(path: &Path, volume: f32) -> Result<(MixerDeviceSink, Player), String> {
    let sink = rodio::DeviceSinkBuilder::open_default_sink()
        .map_err(|_| tr("No se pudo abrir el dispositivo de audio.", "Could not open the audio device.").to_string())?;
    let player = Player::connect_new(&sink.mixer());
    let file = File::open(path).map_err(|_| tr("No se pudo leer el archivo de audio.", "Could not read the audio file.").to_string())?;
    let decoder = Decoder::try_from(file)
        .map_err(|_| tr("Formato de audio no compatible.", "Unsupported audio format.").to_string())?;
    player.set_volume(volume);
    player.append(decoder);
    Ok((sink, player))
}

// ---------------------------------------------------------------------------
// Audio del vídeo (symphonia + rodio, sincronizado con el reloj del vídeo)
// ---------------------------------------------------------------------------

enum VideoAudioCmd {
    Play(bool),
    Seek(f32),
    Volume(f32),
    Stop,
}

#[derive(Default)]
struct VideoAudioState {
    position: f32,
    failed: bool,
}

struct VideoAudio {
    tx: Sender<VideoAudioCmd>,
    state: Arc<Mutex<VideoAudioState>>,
}

impl VideoAudio {
    fn start(path: PathBuf, volume: f32, position: f32) -> Self {
        let (tx, rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(VideoAudioState {
            position,
            failed: false,
        }));
        let thread_state = state.clone();
        std::thread::spawn(move || {
            run_video_audio(path, volume, position, rx, thread_state);
        });
        Self { tx, state }
    }

    fn play(&self, playing: bool) {
        let _ = self.tx.send(VideoAudioCmd::Play(playing));
    }

    fn seek(&self, position: f32) {
        let _ = self.tx.send(VideoAudioCmd::Seek(position.max(0.0)));
    }

    fn set_volume(&self, volume: f32) {
        let _ = self.tx.send(VideoAudioCmd::Volume(volume.clamp(0.0, 1.0)));
    }

    fn position(&self) -> f32 {
        self.state
            .lock()
            .map(|state| state.position)
            .unwrap_or(0.0)
    }

    fn failed(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.failed)
            .unwrap_or(true)
    }
}

impl Drop for VideoAudio {
    fn drop(&mut self) {
        let _ = self.tx.send(VideoAudioCmd::Stop);
    }
}

struct VideoAudioSession {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn SymphoniaDecoder>,
    track_id: u32,
}

fn open_video_audio_session(path: &Path, start: f32) -> Option<VideoAudioSession> {
    let file = File::open(path).ok()?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension() {
        hint.with_extension(&extension.to_string_lossy());
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions {
                prebuild_seek_index: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .ok()?;
    let mut format = probed.format;

    let mut selected: Option<(u32, Box<dyn SymphoniaDecoder>)> = None;
    for track in format.tracks() {
        if track.codec_params.codec == CODEC_TYPE_NULL {
            continue;
        }
        if track.codec_params.sample_rate.is_none() {
            continue;
        }
        let Ok(decoder) =
            symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())
        else {
            continue;
        };
        selected = Some((track.id, decoder));
        break;
    }
    let (track_id, decoder) = selected?;

    if start > 0.05 {
        let seconds = start.max(0.0);
        let time = Time::new(seconds as u64, f64::from(seconds.fract()));
        let _ = format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(track_id),
            },
        );
    }

    Some(VideoAudioSession {
        format,
        decoder,
        track_id,
    })
}

fn run_video_audio(
    path: PathBuf,
    volume: f32,
    start: f32,
    rx: Receiver<VideoAudioCmd>,
    state: Arc<Mutex<VideoAudioState>>,
) {
    let sink = match rodio::DeviceSinkBuilder::open_default_sink() {
        Ok(sink) => sink,
        Err(_) => {
            if let Ok(mut state) = state.lock() {
                state.failed = true;
            }
            return;
        }
    };
    let player = Player::connect_new(&sink.mixer());
    player.set_volume(volume.clamp(0.0, 1.0));

    let Some(mut session) = open_video_audio_session(&path, start) else {
        if let Ok(mut state) = state.lock() {
            state.failed = true;
        }
        return;
    };

    let mut base = start.max(0.0);
    let mut playing = false;
    let mut ended = false;
    let mut pending: Vec<f32> = Vec::new();
    let mut pending_frames = 0usize;
    let mut pending_rate = 0u32;
    let mut pending_channels = 0u16;
    let mut chunk_durations: VecDeque<f32> = VecDeque::new();
    let mut completed = 0.0f32;
    let mut last_audio_pos = 0.0f32;

    'worker: loop {
        loop {
            match rx.try_recv() {
                Ok(VideoAudioCmd::Play(value)) => {
                    playing = value;
                    if playing {
                        player.play();
                    } else {
                        player.pause();
                    }
                }
                Ok(VideoAudioCmd::Seek(position)) => {
                    base = position.max(0.0);
                    ended = false;
                    pending.clear();
                    pending_frames = 0;
                    chunk_durations.clear();
                    completed = 0.0;
                    last_audio_pos = 0.0;
                    player.pause();
                    player.clear();
                    let Some(reopened) = open_video_audio_session(&path, base) else {
                        if let Ok(mut state) = state.lock() {
                            state.failed = true;
                        }
                        return;
                    };
                    session = reopened;
                    if playing {
                        player.play();
                    }
                    if let Ok(mut state) = state.lock() {
                        state.position = base;
                    }
                }
                Ok(VideoAudioCmd::Volume(value)) => {
                    player.set_volume(value.clamp(0.0, 1.0));
                }
                Ok(VideoAudioCmd::Stop) | Err(TryRecvError::Disconnected) => break 'worker,
                Err(TryRecvError::Empty) => break,
            }
        }

        if let Ok(mut state) = state.lock()
            && !state.failed
        {
            let pos = player.get_pos().as_secs_f32();
            if pos + 0.05 < last_audio_pos {
                if let Some(finished) = chunk_durations.pop_front() {
                    completed += finished;
                }
            }
            last_audio_pos = pos;
            state.position = base + completed + pos;
        }

        if !playing {
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }

        // Mantiene como máximo ~0,75 s de audio en cola para no consumir memoria.
        if ended || player.len() >= 3 {
            std::thread::sleep(Duration::from_millis(18));
            continue;
        }

        let target_frames = (pending_rate.max(1) / 4) as usize;
        while pending_frames < target_frames.max(1) && !ended {
            let packet = match session.format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::IoError(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    ended = true;
                    break;
                }
                Err(_) => {
                    ended = true;
                    break;
                }
            };
            if packet.track_id() != session.track_id {
                continue;
            }
            let decoded = match session.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(_) => continue,
            };
            let mut converted =
                AudioBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
            decoded.convert(&mut converted);
            let frames = converted.frames();
            let channels = converted.spec().channels.count();
            let rate = converted.spec().rate;
            if frames == 0 || channels == 0 || rate == 0 {
                continue;
            }
            if pending_rate != 0 && (rate != pending_rate || channels as u16 != pending_channels) {
                if pending_frames > 0 {
                    player.append(SamplesBuffer::new(
                        std::num::NonZeroU16::new(pending_channels.max(1)).unwrap(),
                        std::num::NonZeroU32::new(pending_rate).unwrap(),
                        std::mem::take(&mut pending),
                    ));
                    chunk_durations
                        .push_back(pending_frames as f32 / pending_rate.max(1) as f32);
                    pending_frames = 0;
                }
            }
            pending_rate = rate;
            pending_channels = channels as u16;
            pending.reserve(frames * channels);
            for frame in 0..frames {
                for channel in 0..channels {
                    pending.push(converted.chan(channel).get(frame).copied().unwrap_or(0.0));
                }
            }
            pending_frames += frames;
        }

        if pending_frames > 0 && (pending_frames >= target_frames.max(1) || ended) {
            player.append(SamplesBuffer::new(
                std::num::NonZeroU16::new(pending_channels.max(1)).unwrap(),
                std::num::NonZeroU32::new(pending_rate.max(1)).unwrap(),
                std::mem::take(&mut pending),
            ));
            chunk_durations.push_back(pending_frames as f32 / pending_rate.max(1) as f32);
            pending_frames = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// Vídeo (Media Foundation)
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct VideoSnapshot {
    pub duration: f32,
    pub position: f32,
    pub playing: bool,
    pub error: Option<String>,
}

#[derive(Default)]
struct VideoShared {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    frame_id: u64,
    duration: f32,
    position: f32,
    playing: bool,
    ended: bool,
    ready: bool,
    error: Option<String>,
    seek_to: Option<f32>,
    stop: bool,
}

pub struct VideoPlayer {
    pub frame: Option<Arc<RenderImage>>,
    path: PathBuf,
    shared: Arc<Mutex<VideoShared>>,
    frame_id: u64,
    audio: Option<VideoAudio>,
    volume: f32,
    audio_playing: Option<bool>,
    last_audio_sync: Instant,
}

impl VideoPlayer {
    pub fn open(path: PathBuf, volume: f32, preview_edge: u32) -> Self {
        let shared = Arc::new(Mutex::new(VideoShared::default()));
        let thread_shared = shared.clone();
        let thread_path = path.clone();
        std::thread::spawn(move || run_video(thread_path, thread_shared, preview_edge));
        Self {
            frame: None,
            path,
            shared,
            frame_id: 0,
            audio: None,
            volume: volume.clamp(0.0, 1.0),
            audio_playing: None,
            last_audio_sync: Instant::now(),
        }
    }

    pub fn snapshot(&self) -> VideoSnapshot {
        let shared = match self.shared.lock() {
            Ok(shared) => shared,
            Err(_) => return VideoSnapshot::default(),
        };
        VideoSnapshot {
            duration: shared.duration,
            position: shared.position,
            playing: shared.playing,
            error: shared.error.clone(),
        }
    }

    pub fn is_playing(&self) -> bool {
        self.shared
            .lock()
            .map(|shared| shared.playing)
            .unwrap_or(false)
    }

    /// Convierte el frame pendiente en imagen de GPUI. Devuelve el frame
    /// anterior, si lo había, para que la interfaz lo retire del atlas.
    pub fn sync_frame(&mut self) -> Option<Arc<RenderImage>> {
        let (frame_id, width, height, pixels) = {
            let mut shared = match self.shared.lock() {
                Ok(shared) => shared,
                Err(_) => return None,
            };
            if shared.frame_id == self.frame_id || shared.width == 0 || shared.height == 0 {
                return None;
            }
            (
                shared.frame_id,
                shared.width,
                shared.height,
                std::mem::take(&mut shared.pixels),
            )
        };
        self.frame_id = frame_id;
        let Some(buffer) = image::RgbaImage::from_raw(width, height, pixels) else {
            return None;
        };
        let next = Arc::new(RenderImage::new(smallvec::smallvec![
            image::Frame::new(buffer)
        ]));
        self.frame.replace(next)
    }

    pub fn toggle(&mut self) {
        let start_position;
        let playing = if let Ok(mut shared) = self.shared.lock() {
            if shared.error.is_some() {
                return;
            }
            if shared.ended {
                shared.seek_to = Some(0.0);
                shared.ended = false;
                shared.playing = true;
                start_position = 0.0;
            } else {
                shared.playing = !shared.playing;
                start_position = shared.position;
            }
            shared.playing
        } else {
            return;
        };

        self.ensure_audio(start_position);
        if let Some(audio) = &self.audio {
            if self.audio_playing != Some(playing) {
                audio.play(playing);
                self.audio_playing = Some(playing);
            }
            if playing {
                audio.seek(start_position);
                self.last_audio_sync = Instant::now();
            }
        }
    }

    pub fn seek_ratio(&self, ratio: f32) {
        let mut target = 0.0f32;
        if let Ok(mut shared) = self.shared.lock() {
            let duration = shared.duration;
            if duration <= 0.0 {
                return;
            }
            target = (ratio.clamp(0.0, 1.0) * duration).clamp(0.0, duration);
            shared.seek_to = Some(target);
            shared.position = target;
            shared.ended = false;
        }
        if let Some(audio) = &self.audio {
            audio.seek(target);
        }
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        if let Some(audio) = &self.audio {
            audio.set_volume(self.volume);
        }
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Sincroniza el audio con el reloj del vídeo. Debe llamarse en cada tick.
    pub fn sync_audio(&mut self) {
        if self
            .audio
            .as_ref()
            .map(|audio| audio.failed())
            .unwrap_or(false)
        {
            self.audio = None;
            return;
        }
        let (playing, position) = match self.shared.lock() {
            Ok(shared) => (shared.playing, shared.position),
            Err(_) => return,
        };
        let Some(audio) = &self.audio else {
            return;
        };
        if self.audio_playing != Some(playing) {
            audio.play(playing);
            self.audio_playing = Some(playing);
        }
        if playing {
            let drift = (audio.position() - position).abs();
            if drift > 0.55 && self.last_audio_sync.elapsed() > Duration::from_millis(1500) {
                audio.seek(position);
                self.last_audio_sync = Instant::now();
            }
        }
    }

    fn ensure_audio(&mut self, position: f32) {
        if self.audio.is_some() {
            return;
        }
        let audio = VideoAudio::start(self.path.clone(), self.volume, position);
        self.audio = Some(audio);
        self.audio_playing = None;
    }

    pub fn stop(&self) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.stop = true;
        }
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_video(path: PathBuf, shared: Arc<Mutex<VideoShared>>, preview_edge: u32) {
    unsafe {
        // El hilo del vídeo necesita su propia inicialización COM; sin ella
        // algunos decodificadores de Media Foundation entregan solo keyframes.
        let com = CoInitializeEx(None, COINIT_MULTITHREADED);
        if MFStartup(MF_VERSION, MFSTARTUP_LITE).is_err() {
            set_video_error(&shared, tr("No se pudo iniciar Media Foundation.", "Could not start Media Foundation."));
            if com.is_ok() {
                CoUninitialize();
            }
            return;
        }
        if let Err(error) = play_video(&path, &shared, preview_edge) {
            set_video_error(&shared, &error);
        }
        let _ = MFShutdown();
        if com.is_ok() {
            CoUninitialize();
        }
    }
}

fn set_video_error(shared: &Arc<Mutex<VideoShared>>, message: &str) {
    if let Ok(mut shared) = shared.lock() {
        shared.error = Some(message.to_string());
        shared.playing = false;
        shared.ready = true;
    }
}

unsafe fn play_video(
    path: &Path,
    shared: &Arc<Mutex<VideoShared>>,
    preview_edge: u32,
) -> Result<(), String> {
    let mut attributes = None;
    MFCreateAttributes(&mut attributes, 1)
        .map_err(|_| tr("Media Foundation no disponible.", "Media Foundation is unavailable.").to_string())?;
    let attributes = attributes.ok_or(tr("Media Foundation no disponible.", "Media Foundation is unavailable.").to_string())?;
    // Sin MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING: el procesador de vídeo de
    // Media Foundation descarta frames con fuentes 1080p60. Decodificamos NV12
    // y hacemos la conversión a RGBA (con reducción) nosotros mismos.

    let wide_path = platform::wide(path);
    let reader = MFCreateSourceReaderFromURL(PCWSTR(wide_path.as_ptr()), &attributes)
        .map_err(|_| tr("No se pudo abrir el vídeo.", "Could not open the video.").to_string())?;

    let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
    reader
        .SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, true)
        .map_err(|_| tr("El archivo no contiene vídeo.", "The file does not contain video.").to_string())?;

    let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    let native_type = reader
        .GetNativeMediaType(stream, 0)
        .map_err(|_| tr("El archivo no contiene vídeo.", "The file does not contain video.").to_string())?;
    let (native_width, native_height) = frame_size(&native_type);
    if native_width == 0 || native_height == 0 {
        return Err(tr("No se pudo leer el tamaño del vídeo.", "Could not read the video dimensions.").to_string());
    }
    let (color_matrix, full_range) = yuv_color_info(&native_type, native_height);

    let native_rate = native_type
        .GetUINT64(&MF_MT_FRAME_RATE)
        .ok()
        .filter(|rate| *rate != 0)
        .unwrap_or((30u64 << 32) | 1);

    let output = MFCreateMediaType().map_err(|_| tr("Media Foundation no disponible.", "Media Foundation is unavailable.").to_string())?;
    output
        .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
        .map_err(|_| tr("Formato de vídeo no compatible.", "Unsupported video format.").to_string())?;
    output
        .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
        .map_err(|_| tr("Formato de vídeo no compatible.", "Unsupported video format.").to_string())?;
    output
        .SetUINT64(
            &MF_MT_FRAME_SIZE,
            ((native_width as u64) << 32) | native_height as u64,
        )
        .map_err(|_| tr("Formato de vídeo no compatible.", "Unsupported video format.").to_string())?;
    output
        .SetUINT64(&MF_MT_FRAME_RATE, native_rate)
        .map_err(|_| tr("Formato de vídeo no compatible.", "Unsupported video format.").to_string())?;
    reader
        .SetCurrentMediaType(stream, None, &output)
        .map_err(|_| tr("Formato de vídeo no compatible.", "Unsupported video format.").to_string())?;

    let (mut width, mut height) = (native_width, native_height);
    if let Ok(actual) = reader.GetCurrentMediaType(stream) {
        let (actual_w, actual_h) = frame_size(&actual);
        if actual_w > 0 && actual_h > 0 {
            width = actual_w;
            height = actual_h;
        }
    }

    // Solo se convierte el fotograma al tamaño que necesita el inspector.
    let (preview_width, preview_height) = preview_dimensions(width, height, preview_edge);

    let stride = reader
        .GetCurrentMediaType(stream)
        .ok()
        .and_then(|media_type| media_type.GetUINT32(&MF_MT_DEFAULT_STRIDE).ok())
        .map(|value| value as i32)
        .filter(|value| value.unsigned_abs() >= width)
        .unwrap_or(width as i32);

    let duration = reader
        .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
        .ok()
        .map(|variant| variant_to_f32(&variant))
        .unwrap_or(0.0);

    if let Ok(mut shared) = shared.lock() {
        shared.width = preview_width;
        shared.height = preview_height;
        shared.duration = duration;
        shared.ready = true;
    }

    // Reloj de presentación: (instante real, timestamp del último frame).
    // El decodificador entrega samples mucho más rápido que tiempo real, así
    // que cada frame se espera hasta su marca de tiempo para reproducir a
    // velocidad normal sin depender de la duración reportada por el codec.
    let mut clock: Option<(Instant, i64)> = None;
    let mut force_read = true;
    loop {
        let (stop, seek, playing) = {
            let shared = match shared.lock() {
                Ok(shared) => shared,
                Err(_) => return Ok(()),
            };
            (shared.stop, shared.seek_to, shared.playing)
        };
        if stop {
            return Ok(());
        }

        if let Some(target) = seek {
            if let Ok(mut shared) = shared.lock() {
                shared.seek_to = None;
                shared.ended = false;
                shared.position = target;
            }
            let mut variant = PROPVARIANT::default();
            let prop: &mut windows::Win32::System::Com::StructuredStorage::PROPVARIANT_0_0 =
                &mut *variant.Anonymous.Anonymous;
            prop.vt = VT_I8;
            prop.Anonymous.hVal = (target as f64 * 10_000_000.0) as i64;
            let _ = reader.SetCurrentPosition(&GUID::zeroed(), &variant);
            let _ = reader.Flush(stream);
            clock = None;
            force_read = true;
            continue;
        }

        if !playing && !force_read {
            clock = None;
            std::thread::sleep(Duration::from_millis(30));
            continue;
        }

        let mut actual_stream = 0u32;
        let mut flags = 0u32;
        let mut timestamp = 0i64;
        let mut sample: Option<IMFSample> = None;
        let read = reader.ReadSample(
            stream,
            0,
            Some(&mut actual_stream),
            Some(&mut flags),
            Some(&mut timestamp),
            Some(&mut sample),
        );
        if read.is_err() {
            return Err(tr("No se pudo leer el vídeo.", "Could not read the video.").to_string());
        }

        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            if let Ok(mut shared) = shared.lock() {
                shared.ended = true;
                shared.playing = false;
                shared.position = shared.duration;
            }
            clock = None;
            force_read = false;
            std::thread::sleep(Duration::from_millis(40));
            continue;
        }

        // El flag CURRENTMEDIATYPECHANGED puede repetirse en cada muestra. No
        // hay que volver a aplicar el tipo de salida: hacerlo vacía el
        // decodificador y este se salta todos los frames hasta el siguiente
        // keyframe. El tipo ya se fijó antes de leer.

        if let Some(sample) = sample {
            let mut publish = true;
            if playing {
                match clock {
                    None => clock = Some((Instant::now(), timestamp)),
                    Some((wall, media)) => {
                        let ahead = timestamp.saturating_sub(media).max(0) as u64;
                        let target = wall + Duration::from_nanos(ahead.saturating_mul(100));
                        let now = Instant::now();
                        if target > now {
                            let wait = target - now;
                            if wait <= Duration::from_millis(2000) {
                                std::thread::sleep(wait);
                            } else {
                                // Salto enorme en las marcas de tiempo: reanclar.
                                clock = Some((now, timestamp));
                            }
                        } else {
                            let late = now.duration_since(target);
                            if late > Duration::from_millis(1500) {
                                // Muy retrasados: reanclar en vez de arrastrar el error.
                                clock = Some((now, timestamp));
                            } else if late > Duration::from_millis(160) {
                                // Vamos con retraso: descartar el frame para
                                // mantener el reloj y el audio sincronizados.
                                publish = false;
                            }
                        }
                    }
                }
            } else {
                clock = None;
            }

            if publish {
                if let Some(pixels) =
                    copy_sample_nv12(&sample, width, height, stride, preview_width, preview_height, color_matrix, full_range)
                {
                    if let Ok(mut shared) = shared.lock() {
                        shared.pixels = pixels;
                        shared.frame_id = shared.frame_id.wrapping_add(1);
                        shared.position = (timestamp as f32 / 10_000_000.0).max(0.0);
                    }
                }
            }
            force_read = false;
        }
    }
}

fn preview_dimensions(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    let source_edge = width.max(height).max(1) as u64;
    let limit = source_edge.min(u64::from(max_edge));
    let dimension = |value| ((u64::from(value) * limit).div_ceil(source_edge) as u32).max(2);
    (dimension(width), dimension(height))
}

#[cfg(test)]
mod preview_size_tests {
    use super::preview_dimensions;

    #[test]
    fn bounds_preview_memory_in_both_orientations() {
        assert_eq!(preview_dimensions(3840, 2160, 480), (480, 270));
        assert_eq!(preview_dimensions(2160, 3840, 480), (270, 480));
        assert_eq!(preview_dimensions(320, 240, 480), (320, 240));
    }
}

unsafe fn frame_size(media_type: &IMFMediaType) -> (u32, u32) {
    let packed = media_type.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
    ((packed >> 32) as u32, (packed & 0xffff_ffff) as u32)
}

pub fn video_dimensions(path: &Path) -> Option<(u32, u32)> {
    unsafe {
        let com = CoInitializeEx(None, COINIT_MULTITHREADED);
        let value = if MFStartup(MF_VERSION, MFSTARTUP_LITE).is_ok() {
            let result = (|| {
                let path = platform::wide(path);
                let reader = MFCreateSourceReaderFromURL(PCWSTR(path.as_ptr()), None::<&windows::Win32::Media::MediaFoundation::IMFAttributes>).ok()?;
                let video = reader.GetNativeMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, 0).ok()?;
                let size = video.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
                Some(((size >> 32) as u32, size as u32))
            })();
            let _ = MFShutdown();
            result
        } else { None };
        if com.is_ok() { CoUninitialize(); }
        value
    }
}

unsafe fn variant_to_f32(variant: &PROPVARIANT) -> f32 {
    let value = variant.Anonymous.Anonymous.Anonymous.uhVal;
    (value as f32 / 10_000_000.0).max(0.0)
}

#[derive(Clone, Copy)]
enum YuvMatrix {
    Bt601,
    Bt709,
    Bt2020,
}

/// Deduce la matriz de color y el rango del tipo nativo del archivo. Muchos
/// contenedores no declaran `MF_MT_YUV_MATRIX`; en ese caso usamos la función
/// de transferencia (SMPTE 170M/gamma 2.2 => BT.601, BT.709 => BT.709) y, si
/// tampoco hay datos, la resolución (SD => BT.601, HD => BT.709).
unsafe fn yuv_color_info(media_type: &IMFMediaType, height: u32) -> (YuvMatrix, bool) {
    let matrix = media_type.GetUINT32(&MF_MT_YUV_MATRIX).unwrap_or(0);
    let transfer = media_type.GetUINT32(&MF_MT_TRANSFER_FUNCTION).unwrap_or(0);
    let range = media_type.GetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE).unwrap_or(0);
    let full_range = range == MFNominalRange_0_255.0 as u32;
    let matrix = if matrix == MFVideoTransferMatrix_BT709.0 as u32 {
        YuvMatrix::Bt709
    } else if matrix == MFVideoTransferMatrix_BT601.0 as u32 {
        YuvMatrix::Bt601
    } else if matrix == MFVideoTransferMatrix_BT2020_10.0 as u32 {
        YuvMatrix::Bt2020
    } else if transfer == MFVideoTransFunc_709.0 as u32
        || transfer == MFVideoTransFunc_709_sym.0 as u32
    {
        YuvMatrix::Bt709
    } else if transfer == MFVideoTransFunc_22.0 as u32
        || transfer == MFVideoTransFunc_240M.0 as u32
    {
        YuvMatrix::Bt601
    } else if transfer == MFVideoTransFunc_2020.0 as u32
        || transfer == MFVideoTransFunc_2020_const.0 as u32
        || transfer == MFVideoTransFunc_2084.0 as u32
        || transfer == MFVideoTransFunc_HLG.0 as u32
    {
        YuvMatrix::Bt2020
    } else if height <= 576 {
        YuvMatrix::Bt601
    } else {
        YuvMatrix::Bt709
    };
    (matrix, full_range)
}

unsafe fn copy_sample_nv12(
    sample: &IMFSample,
    width: u32,
    height: u32,
    stride: i32,
    target_width: u32,
    target_height: u32,
    matrix: YuvMatrix,
    full_range: bool,
) -> Option<Vec<u8>> {
    let buffer = sample.ConvertToContiguousBuffer().ok()?;
    let mut pointer: *mut u8 = std::ptr::null_mut();
    let mut length = 0u32;
    buffer
        .Lock(&mut pointer, None, Some(&mut length))
        .ok()?;
    if pointer.is_null() || length == 0 {
        let _ = buffer.Unlock();
        return None;
    }

    let width = width as usize;
    let height = height as usize;
    let stride_abs = stride.unsigned_abs() as usize;
    let aligned_height = (height + 15) & !15;
    let chroma_height = height.div_ceil(2);
    let source = std::slice::from_raw_parts(pointer, length as usize);
    // Algunos decodificadores alinean la altura del plano Y (1080 -> 1088)
    // antes de empezar el plano UV. Detectamos el desplazamiento real usando
    // el tamaño del búfer.
    let uv_offset = if source.len() >= stride_abs * aligned_height + stride_abs * chroma_height {
        stride_abs * aligned_height
    } else {
        stride_abs * height
    };
    if stride_abs < width || source.len() < uv_offset + stride_abs * chroma_height {
        let _ = buffer.Unlock();
        return None;
    }

    let target_width = target_width.max(2) as usize;
    let target_height = target_height.max(2) as usize;
    let mut pixels = vec![0u8; target_width * target_height * 4];
    let bottom_up = stride < 0;
    let max_uv_x = stride_abs.saturating_sub(2);
    let (luma_offset, luma_scale, rv, gu, gv, bu) = match (matrix, full_range) {
        (YuvMatrix::Bt601, false) => (16.0, 1.164, 1.596, 0.391, 0.813, 2.018),
        (YuvMatrix::Bt601, true) => (0.0, 1.0, 1.402, 0.344, 0.714, 1.772),
        (YuvMatrix::Bt709, false) => (16.0, 1.164, 1.793, 0.213, 0.533, 2.112),
        (YuvMatrix::Bt709, true) => (0.0, 1.0, 1.575, 0.187, 0.468, 1.856),
        (YuvMatrix::Bt2020, false) => (16.0, 1.164, 1.679, 0.187, 0.650, 2.142),
        (YuvMatrix::Bt2020, true) => (0.0, 1.0, 1.475, 0.165, 0.571, 1.881),
    };

    // GPUI interpreta los bytes de RenderImage como BGRA. Escribimos B, G, R, A
    // para que el frame de vídeo se muestre con los colores correctos.
    for y in 0..target_height {
        let source_y = (y * height / target_height).min(height - 1);
        let luma_y = if bottom_up {
            height - 1 - source_y
        } else {
            source_y
        };
        let luma_base = luma_y * stride_abs;
        let chroma_y = if bottom_up {
            chroma_height - 1 - source_y / 2
        } else {
            source_y / 2
        };
        let chroma_base = uv_offset + chroma_y * stride_abs;

        for x in 0..target_width {
            let source_x = (x * width / target_width).min(width - 1);
            let luma = source[luma_base + source_x] as f32;
            let uv_x = ((source_x / 2) * 2).min(max_uv_x);
            let u = source[chroma_base + uv_x] as f32 - 128.0;
            let v = source[chroma_base + uv_x + 1] as f32 - 128.0;

            let luma = (luma - luma_offset).max(0.0) * luma_scale;
            let r = (luma + rv * v).clamp(0.0, 255.0) as u8;
            let g = (luma - gu * u - gv * v).clamp(0.0, 255.0) as u8;
            let b = (luma + bu * u).clamp(0.0, 255.0) as u8;

            let index = (y * target_width + x) * 4;
            pixels[index] = b;
            pixels[index + 1] = g;
            pixels[index + 2] = r;
            pixels[index + 3] = 255;
        }
    }

    let _ = buffer.Unlock();
    Some(pixels)
}
