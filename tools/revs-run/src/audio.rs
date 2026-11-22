//! Sound (Phase 3.5): an SDL2 output queue fed from the run loop.
//!
//! - Sound effects: the game opens a RemoteIO AudioUnit and mixes its own
//!   PCM in a render callback. The pump calls that callback (guest code, so
//!   only on the main thread) for as many frames as the output needs.
//! - Music: AVAudioPlayer gets MP3 data; it is decoded once (symphonia) and
//!   mixed here with its volume and loop count.
//!
//! The output is 44.1 kHz stereo i16; other rates are resampled (linear).
//! `--mute` keeps everything running but sends silence.

use symphonia::core::io::MediaSourceStream;

use crate::objc::{self, Msg, NativeImp, Obj};
use crate::os::arg;
use crate::{Cpu, Host};

const OUT_RATE: u32 = 44100;
/// Keep about this many frames queued (~70 ms).
const TARGET: u32 = 3072;
/// Most frames asked of the game's render callback at once.
const CHUNK: u32 = 512;

/// The handle the game gets for the one RemoteIO unit.
const UNIT: u32 = 0xA0D1_0001;
const COMPONENT: u32 = 0xA0D1_0000;

/// Every C function `call` handles (for linking).
pub const NATIVE: &[&str] = &[
    "AudioComponentFindNext", "AudioComponentInstanceNew", "AudioComponentInstanceDispose", "AudioUnitInitialize",
    "AudioUnitUninitialize", "AudioUnitSetProperty", "AudioUnitGetProperty", "AudioOutputUnitStart",
    "AudioOutputUnitStop", "AudioSessionInitialize", "AudioSessionSetActive", "AudioSessionSetProperty",
    "AudioSessionGetProperty", "AudioSessionAddPropertyListener",
];

#[derive(Clone, Copy, Default, Debug)]
struct Format {
    rate: f64,
    format_id: u32,
    flags: u32,
    bytes_per_frame: u32,
    channels: u32,
    bits: u32,
}

struct Player {
    obj: u32,
    /// Interleaved i16.
    pcm: Vec<i16>,
    rate: u32,
    channels: u32,
    /// Position in source frames.
    pos: f64,
    playing: bool,
    volume: f32,
    /// -1 = forever.
    loops: i32,
    delegate: u32,
}

pub struct Audio {
    out: Option<sdl2::audio::AudioQueue<i16>>,
    failed: bool,
    pub mute: bool,
    unit_format: Format,
    /// Formats the game set on the input / output scope (touchHLE's rules:
    /// input set -> 1 buffer at its rate; only output set -> 2 buffers with
    /// the same data, at the hardware rate, as Capcom's RE engine expects).
    input_format: Option<Format>,
    output_format: Option<Format>,
    callback: u32,
    refcon: u32,
    running: bool,
    /// Guest buffers for the callback: AudioBufferList, data, flags, timestamp.
    bufs: Option<(u32, u32, u32, u32)>,
    sample_time: f64,
    /// Game frames (resampled) not yet sent.
    pending: Vec<(i16, i16)>,
    phase: f64,
    players: Vec<Player>,
    pub frames_out: u64,
    pub peak: i16,
    pub log: Vec<String>,
}

impl Audio {
    pub fn new() -> Audio {
        Audio {
            out: None,
            failed: false,
            mute: false,
            unit_format: Format { rate: 44100.0, format_id: 0x6C70636D, flags: 0xC, bytes_per_frame: 4, channels: 2, bits: 16 },
            input_format: None,
            output_format: None,
            callback: 0,
            refcon: 0,
            running: false,
            bufs: None,
            sample_time: 0.0,
            pending: Vec::new(),
            phase: 0.0,
            players: Vec::new(),
            frames_out: 0,
            peak: 0,
            log: Vec::new(),
        }
    }
}

/// MP3 (or another symphonia format) to interleaved i16: (samples, rate, channels).
fn decode(data: Vec<u8>) -> Result<(Vec<i16>, u32, u32), String> {
    let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(data)), Default::default());
    let mut probed = symphonia::default::get_probe()
        .probe(&Default::default(), mss, Default::default(), Default::default())
        .map_err(|e| format!("probe: {e}"))?;
    let track = probed
        .tracks()
        .iter()
        .find(|t| t.codec_params.as_ref().and_then(|p| p.audio()).is_some())
        .ok_or("no audio track")?;
    let id = track.id;
    let params = track.codec_params.as_ref().unwrap().audio().unwrap().clone();
    let mut dec = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &Default::default())
        .map_err(|e| format!("decoder: {e}"))?;
    let (mut out, mut rate, mut channels) = (Vec::<i16>::new(), 0, 0);
    let mut tmp = Vec::<u8>::new();
    loop {
        let packet = match probed.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) | Err(symphonia::core::errors::Error::IoError(_)) => break,
            Err(e) => return Err(format!("read: {e}")),
        };
        if packet.track_id != id {
            continue;
        }
        let Ok(buf) = dec.decode(&packet) else { continue };
        rate = buf.spec().rate();
        channels = buf.spec().channels().count() as u32;
        tmp.clear();
        buf.copy_bytes_to_vec_interleaved_as::<i16>(&mut tmp);
        out.extend(tmp.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
    }
    if rate == 0 {
        return Err("no audio decoded".into());
    }
    Ok((out, rate, channels))
}

fn read_format(h: &Host, a: u32) -> Format {
    Format {
        rate: f64::from_bits(h.rd32(a) as u64 | (h.rd32(a + 4) as u64) << 32),
        format_id: h.rd32(a + 8),
        flags: h.rd32(a + 12),
        bytes_per_frame: h.rd32(a + 24),
        channels: h.rd32(a + 28),
        bits: h.rd32(a + 32),
    }
}

fn write_format(h: &Host, a: u32, f: &Format) {
    let r = f.rate.to_bits();
    for (k, v) in [r as u32, (r >> 32) as u32, f.format_id, f.flags, f.bytes_per_frame, 1, f.bytes_per_frame, f.channels, f.bits, 0]
        .iter()
        .enumerate()
    {
        h.wr32(a + 4 * k as u32, *v);
    }
}

/// Calls a guest function with 6 arguments (4 in registers, 2 on the stack).
fn call6(h: &Host, c: &mut Cpu, f: u32, a: [u32; 6]) -> u32 {
    let sp = c.r[13];
    c.r[13] = sp - 16;
    h.wr32(sp - 16, a[4]);
    h.wr32(sp - 12, a[5]);
    let r = crate::guest_call(c, f, &a[..4]);
    c.r[13] = sp;
    r
}

/// Asks the game's render callback for `n` frames; appends them to `pending`
/// resampled to the output rate.
fn render_game(h: &mut Host, c: &mut Cpu, n: u32) {
    let a = &h.audio;
    let f = a.input_format.or(a.output_format).unwrap_or(a.unit_format);
    let two = a.input_format.is_none() && a.output_format.is_some();
    let rate = if a.input_format.is_some() { f.rate } else { OUT_RATE as f64 };
    let ch = f.channels.clamp(1, 2);
    if h.audio.bufs.is_none() {
        // List with room for 2 buffers; data = 2 x CHUNK frames x 2 ch x i16.
        let list = crate::os::alloc_zeroed(h, 28);
        let data = crate::os::alloc_zeroed(h, CHUNK * 8);
        let flags = crate::os::alloc_zeroed(h, 4);
        let ts = crate::os::alloc_zeroed(h, 64);
        h.audio.bufs = Some((list, data, flags, ts));
    }
    let (list, data, flags, ts) = h.audio.bufs.unwrap();
    let ratio = rate.max(1.0) / OUT_RATE as f64;
    let want = ((n as f64) * ratio).ceil() as u32;
    let mut got: Vec<(i16, i16)> = Vec::with_capacity(want as usize);
    let mut left = want;
    while left > 0 {
        let k = left.min(CHUNK);
        let bytes = k * ch * 2;
        // Interleaved buffer(s); the second one (if any) is ignored afterwards.
        h.wr32(list, if two { 2 } else { 1 });
        h.wr32(list + 4, ch);
        h.wr32(list + 8, bytes);
        h.wr32(list + 12, data);
        h.wr32(list + 16, ch);
        h.wr32(list + 20, bytes);
        h.wr32(list + 24, data + CHUNK * 4);
        h.wr32(flags, 0);
        let st = h.audio.sample_time.to_bits();
        h.wr32(ts, st as u32);
        h.wr32(ts + 4, (st >> 32) as u32);
        h.wr32(ts + 56, 1); // kAudioTimeStampSampleTimeValid
        call6(h, c, h.audio.callback, [h.audio.refcon, flags, ts, 0, k, list]);
        if c.halt != 0 {
            return;
        }
        for i in 0..k {
            let s = |j: u32| { let a = data + 2 * (i * ch + j); i16::from_le_bytes([h.rd8(a), h.rd8(a + 1)]) };
            got.push(if ch == 2 { (s(0), s(1)) } else { (s(0), s(0)) });
        }
        h.audio.sample_time += k as f64;
        left -= k;
    }
    // Resample to the output rate (linear).
    let a = &mut h.audio;
    let mut p = a.phase;
    while (p as usize) + 1 < got.len() && a.pending.len() < (n as usize) * 2 {
        let i = p as usize;
        let t = (p - i as f64) as f32;
        let lerp = |x: i16, y: i16| (x as f32 + (y as f32 - x as f32) * t) as i16;
        a.pending.push((lerp(got[i].0, got[i + 1].0), lerp(got[i].1, got[i + 1].1)));
        p += ratio;
    }
    a.phase = p - (got.len().saturating_sub(1)) as f64;
    if a.phase < 0.0 {
        a.phase = 0.0;
    }
}

/// Feeds the output: called from the run loop on every pass.
pub fn pump(h: &mut Host, c: &mut Cpu) {
    if h.audio.out.is_none() && !h.audio.failed {
        let Some(g) = h.gfx.as_ref() else { return };
        match g.window.open_audio(OUT_RATE as i32, 2) {
            Ok(q) => h.audio.out = Some(q),
            Err(e) => {
                h.audio.failed = true;
                h.audio.log.push(format!("audio output: {e}"));
                return;
            }
        }
    }
    let Some(q) = h.audio.out.as_ref() else { return };
    let queued = q.size() / 4;
    if queued >= TARGET {
        return;
    }
    let n = TARGET * 2 - queued;
    let mut mix = vec![(0i32, 0i32); n as usize];
    // Sound effects from the game's mixer.
    if h.audio.running && h.audio.callback != 0 {
        while h.audio.pending.len() < n as usize {
            let before = h.audio.pending.len();
            render_game(h, c, n - before as u32);
            if c.halt != 0 || h.audio.pending.len() == before {
                break;
            }
        }
        let take = h.audio.pending.len().min(n as usize);
        for (m, s) in mix.iter_mut().zip(h.audio.pending.drain(..take)) {
            m.0 += s.0 as i32;
            m.1 += s.1 as i32;
        }
    }
    // Music.
    let mut finished = Vec::new();
    for p in h.audio.players.iter_mut().filter(|p| p.playing) {
        let ch = p.channels.max(1) as usize;
        let frames = p.pcm.len() / ch;
        let step = p.rate as f64 / OUT_RATE as f64;
        let vol = p.volume.clamp(0.0, 1.0);
        for m in mix.iter_mut() {
            if p.pos as usize >= frames {
                if p.loops != 0 {
                    p.loops -= (p.loops > 0) as i32;
                    p.pos = 0.0;
                } else {
                    p.playing = false;
                    p.pos = 0.0;
                    finished.push((p.obj, p.delegate));
                    break;
                }
            }
            let i = p.pos as usize * ch;
            let (l, r) = (p.pcm[i], p.pcm[i + ch - 1]);
            m.0 += (l as f32 * vol) as i32;
            m.1 += (r as f32 * vol) as i32;
            p.pos += step;
        }
    }
    let mut out = Vec::with_capacity(n as usize * 2);
    for (l, r) in mix {
        let (l, r) = (l.clamp(-32768, 32767) as i16, r.clamp(-32768, 32767) as i16);
        h.audio.peak = h.audio.peak.max(l.saturating_abs()).max(r.saturating_abs());
        if h.audio.mute {
            out.extend([0, 0]);
        } else {
            out.extend([l, r]);
        }
    }
    h.audio.frames_out += n as u64;
    let _ = h.audio.out.as_ref().unwrap().queue_audio(&out);
    for (obj, d) in finished {
        if d != 0 && objc::responds(h, d, "audioPlayerDidFinishPlaying:successfully:") {
            objc::send(h, c, d, "audioPlayerDidFinishPlaying:successfully:", &[obj, 1]);
        }
    }
}

/// AudioUnit / AudioSession C functions.
pub fn call(h: &mut Host, c: &mut Cpu, name: &str) -> bool {
    if !NATIVE.contains(&name) {
        return false;
    }
    let (a0, a1, a2, a3, a4) = (arg(h, c, 0), arg(h, c, 1), arg(h, c, 2), arg(h, c, 3), arg(h, c, 4));
    let r0 = match name {
        "AudioComponentFindNext" => COMPONENT,
        "AudioComponentInstanceNew" => {
            h.wr32(a1, UNIT);
            0
        }
        "AudioUnitSetProperty" => {
            match a1 {
                // kAudioUnitProperty_StreamFormat
                8 => {
                    let f = read_format(h, a4);
                    h.audio.log.push(format!("AudioUnit format (scope {a2}, bus {a3}): {f:?}"));
                    match a2 {
                        1 => h.audio.input_format = Some(f),
                        2 => h.audio.output_format = Some(f),
                        _ => h.audio.unit_format = f,
                    }
                }
                // kAudioUnitProperty_SetRenderCallback
                23 => {
                    h.audio.callback = h.rd32(a4);
                    h.audio.refcon = h.rd32(a4 + 4);
                    h.audio.log.push(format!("AudioUnit render callback {:#x}", h.audio.callback));
                }
                _ => {}
            }
            0
        }
        "AudioUnitGetProperty" => {
            if a1 == 8 && a4 != 0 {
                let f = match a2 {
                    1 => h.audio.input_format,
                    2 => h.audio.output_format,
                    _ => None,
                }
                .unwrap_or(h.audio.unit_format);
                write_format(h, a4, &f);
            }
            0
        }
        "AudioOutputUnitStart" => {
            h.audio.running = true;
            0
        }
        "AudioOutputUnitStop" => {
            h.audio.running = false;
            0
        }
        "AudioSessionGetProperty" => {
            // (id, *size, *data)
            let size = if a1 != 0 { h.rd32(a1) } else { 4 };
            match a0 {
                // kAudioSessionProperty_CurrentHardwareSampleRate 'chsr' (Float64)
                0x63687372 if size >= 8 => {
                    let b = (OUT_RATE as f64).to_bits();
                    h.wr32(a2, b as u32);
                    h.wr32(a2 + 4, (b >> 32) as u32);
                }
                // 'chou' output volume (Float32)
                0x63686f75 => h.wr32(a2, 1.0f32.to_bits()),
                _ => {
                    for k in 0..size.min(16) {
                        h.wr8(a2 + k, 0);
                    }
                }
            }
            0
        }
        _ => 0,
    };
    c.r[0] = r0;
    true
}

fn player<'a>(h: &'a mut Host, o: u32) -> Option<&'a mut Player> {
    h.audio.players.iter_mut().find(|p| p.obj == o)
}

fn new_player(h: &mut Host, this: u32, data: Vec<u8>) -> u32 {
    match decode(data) {
        Ok((pcm, rate, channels)) => {
            let secs = pcm.len() as f64 / channels.max(1) as f64 / rate as f64;
            h.audio.log.push(format!("AVAudioPlayer: {rate} Hz, {channels} ch, {secs:.1} s"));
            h.audio.players.retain(|p| p.obj != this);
            h.audio.players.push(Player { obj: this, pcm, rate, channels, pos: 0.0, playing: false, volume: 1.0, loops: 0, delegate: 0 });
            this
        }
        Err(e) => {
            h.audio.log.push(format!("AVAudioPlayer: can't decode ({e})"));
            0
        }
    }
}

fn data_of(h: &Host, o: u32) -> Vec<u8> {
    match h.objc.objs.get(&o) {
        Some(Obj::Data { ptr, len }) => unsafe { std::slice::from_raw_parts(h.at(*ptr), *len as usize) }.to_vec(),
        _ => Vec::new(),
    }
}

pub fn native_imp(class: &str, meta: bool, s: &str) -> Option<NativeImp> {
    if class != "AVAudioPlayer" || meta {
        return crate::touch::native_imp(class, meta, s);
    }
    Some(match s {
        "initWithData:error:" => |h: &mut Host, c: &mut Cpu, m: &Msg| {
            let d = data_of(h, m.arg(h, c, 0));
            new_player(h, m.this, d)
        },
        "initWithContentsOfURL:error:" => |h, c, m| {
            let u = m.arg(h, c, 0);
            let p = objc::string_of(h, u).unwrap_or_default();
            let p = p.strip_prefix("file://").unwrap_or(&p).to_string();
            match h.fs.read_all(&p) {
                Some(d) => new_player(h, m.this, d),
                None => 0,
            }
        },
        "prepareToPlay" => |_, _, _| 1,
        "play" => |h, _, m| {
            if let Some(p) = player(h, m.this) {
                p.playing = true;
                1
            } else {
                0
            }
        },
        "pause" => |h, _, m| {
            if let Some(p) = player(h, m.this) {
                p.playing = false;
            }
            0
        },
        "stop" => |h, _, m| {
            // AVAudioPlayer's stop keeps the position.
            if let Some(p) = player(h, m.this) {
                p.playing = false;
            }
            0
        },
        "isPlaying" => |h, _, m| player(h, m.this).is_some_and(|p| p.playing) as u32,
        "setVolume:" => |h, c, m| {
            let v = f32::from_bits(m.arg(h, c, 0));
            if let Some(p) = player(h, m.this) {
                p.volume = v;
            }
            0
        },
        "volume" => |h, _, m| player(h, m.this).map_or(0.0f32, |p| p.volume).to_bits(),
        "setNumberOfLoops:" => |h, c, m| {
            let n = m.arg(h, c, 0) as i32;
            if let Some(p) = player(h, m.this) {
                p.loops = n;
            }
            0
        },
        "numberOfLoops" => |h, _, m| player(h, m.this).map_or(0, |p| p.loops) as u32,
        "setDelegate:" => |h, c, m| {
            let d = m.arg(h, c, 0);
            if let Some(p) = player(h, m.this) {
                p.delegate = d;
            }
            0
        },
        "delegate" => |h, _, m| player(h, m.this).map_or(0, |p| p.delegate),
        "currentTime" | "duration" => |h, c, m| {
            let dur = objc::sel_name(h, m.sel) == "duration";
            let v = player(h, m.this).map_or(0.0, |p| {
                if dur {
                    p.pcm.len() as f64 / p.channels.max(1) as f64 / p.rate as f64
                } else {
                    p.pos / p.rate as f64
                }
            });
            let b = v.to_bits();
            c.r[1] = (b >> 32) as u32;
            b as u32
        },
        "setCurrentTime:" => |h, c, m| {
            let t = f64::from_bits(m.arg(h, c, 0) as u64 | (m.arg(h, c, 1) as u64) << 32);
            if let Some(p) = player(h, m.this) {
                p.pos = (t.max(0.0) * p.rate as f64).floor();
            }
            0
        },
        "dealloc" => |h, _, m| {
            h.audio.players.retain(|p| p.obj != m.this);
            0
        },
        _ => return crate::touch::native_imp(class, meta, s),
    })
}
