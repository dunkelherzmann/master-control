//! macOS: what the machine plays, from ScreenCaptureKit's audio of the main display (macOS 13 and later). It needs
//! the Screen Recording permission, which the master already has for pictures, and no sound driver of its own.
//! ScreenCaptureKit hands sound over only while something plays, stamped with the host clock, so the gaps are filled
//! with silence here to keep the recording's times true.
// objc 0.2's macros test a cfg Cargo doesn't declare.
#![allow(unexpected_cfgs)]

use std::ffi::c_void;
use std::io;
use std::os::raw::c_char;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Protocol, Sel, BOOL, YES};

use super::{Placer, Sink};

type Id = *mut Object;

const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
/// Silence is added only up to this long ago, so sound that is still on its way lands in its place.
const LATE_S: f64 = 0.2;
const AUDIO: isize = 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

#[repr(C)]
struct AudioStreamBasicDescription {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

#[repr(C)]
struct AudioBuffer {
    channels: u32,
    byte_size: u32,
    data: *mut c_void,
}

const FLOAT: u32 = 1;
const NON_INTERLEAVED: u32 = 1 << 5;

#[link(name = "ScreenCaptureKit", kind = "framework")]
extern "C" {}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetPresentationTimeStamp(buffer: *const c_void) -> CMTime;
    fn CMSampleBufferGetNumSamples(buffer: *const c_void) -> isize;
    fn CMSampleBufferGetFormatDescription(buffer: *const c_void) -> *const c_void;
    fn CMAudioFormatDescriptionGetStreamBasicDescription(description: *const c_void) -> *const AudioStreamBasicDescription;
    fn CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        buffer: *const c_void,
        size_needed: *mut usize,
        list: *mut c_void,
        size: usize,
        allocator: *const c_void,
        memory_allocator: *const c_void,
        flags: u32,
        block_buffer: *mut *const c_void,
    ) -> i32;
    fn CMClockGetHostTimeClock() -> *const c_void;
    fn CMClockGetTime(clock: *const c_void) -> CMTime;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(object: *const c_void);
}

extern "C" {
    fn dispatch_queue_create(label: *const c_char, attributes: *const c_void) -> *mut c_void;
}

// ------------------------------------------------------------------------------------------- completion blocks

/// A block that captures nothing, laid out as the compiler lays out a global block: it is never copied or freed,
/// and what it is told goes into [`ANSWERS`].
#[repr(C)]
struct GlobalBlock {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: *const c_void,
    descriptor: *const BlockDescriptor,
}

#[repr(C)]
struct BlockDescriptor {
    reserved: usize,
    size: usize,
}

extern "C" {
    static _NSConcreteGlobalBlock: c_void;
}

const BLOCK_IS_GLOBAL: i32 = 1 << 28;
static DESCRIPTOR: BlockDescriptor = BlockDescriptor { reserved: 0, size: std::mem::size_of::<GlobalBlock>() };

/// What ScreenCaptureKit's completion handlers said: the shareable content (or why not), and the start and stop.
#[derive(Default)]
struct Answers {
    content: Option<Result<usize, String>>,
    started: Option<Option<String>>,
    stopped: bool,
}

static ANSWERS: Mutex<Answers> = Mutex::new(Answers { content: None, started: None, stopped: false });

extern "C" fn content_handler(_block: *const c_void, content: Id, error: Id) {
    let answer = if content.is_null() {
        Err(description_of(error))
    } else {
        // SAFETY: the content object is kept until this process ends.
        let _: Id = unsafe { msg_send![content, retain] };
        Ok(content as usize)
    };
    ANSWERS.lock().unwrap().content = Some(answer);
}

extern "C" fn started_handler(_block: *const c_void, error: Id) {
    ANSWERS.lock().unwrap().started = Some(if error.is_null() { None } else { Some(description_of(error)) });
}

extern "C" fn stopped_handler(_block: *const c_void, _error: Id) {
    ANSWERS.lock().unwrap().stopped = true;
}

fn global_block(invoke: *const c_void) -> GlobalBlock {
    let isa = std::ptr::addr_of!(_NSConcreteGlobalBlock);
    GlobalBlock { isa, flags: BLOCK_IS_GLOBAL, reserved: 0, invoke, descriptor: &DESCRIPTOR }
}

/// Waits up to `limit` for `answered` to find an answer in [`ANSWERS`].
fn wait_for<T>(limit: Duration, mut answered: impl FnMut(&mut Answers) -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(answer) = answered(&mut ANSWERS.lock().unwrap()) {
            return Some(answer);
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn seconds(time: CMTime) -> f64 {
    if time.timescale == 0 {
        0.0
    } else {
        time.value as f64 / f64::from(time.timescale)
    }
}

fn host_now() -> f64 {
    // SAFETY: plain calls into CoreMedia; the host clock is a shared object that isn't released.
    unsafe { seconds(CMClockGetTime(CMClockGetHostTimeClock())) }
}

/// What the capture's callbacks and the helper's loop share.
struct Shared {
    /// The host time the recording starts at.
    origin_s: f64,
    placer: Placer,
    error: Option<String>,
}

static SHARED: Mutex<Option<Shared>> = Mutex::new(None);

/// A sample buffer's sound, interleaved, as two channels.
///
/// # Safety
/// `buffer` must be a valid CMSampleBuffer of audio.
unsafe fn samples_of(buffer: *const c_void) -> Result<Vec<f32>, String> {
    let description = CMAudioFormatDescriptionGetStreamBasicDescription(CMSampleBufferGetFormatDescription(buffer));
    let description = description.as_ref().ok_or("a sound buffer came without its format")?;
    if description.format_flags & FLOAT == 0 || description.bits_per_channel != 32 {
        return Err(format!("ScreenCaptureKit sent sound in a format not read here (flags {:#x}, {} bits)", description.format_flags, description.bits_per_channel));
    }
    if description.sample_rate.round() as u32 != RATE {
        return Err(format!("ScreenCaptureKit sent sound at {} Hz, not the {RATE} Hz asked for", description.sample_rate));
    }
    let frames = CMSampleBufferGetNumSamples(buffer).max(0) as usize;
    let source_channels = description.channels_per_frame.max(1) as usize;
    // First the size the list needs, then the list.
    let mut needed = 0usize;
    let _ = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(buffer, &mut needed, std::ptr::null_mut(), 0, std::ptr::null(), std::ptr::null(), 1, std::ptr::null_mut());
    if needed == 0 {
        return Err("the sound's buffer list has no size".into());
    }
    let mut list = vec![0u64; needed.div_ceil(8)];
    let mut block_buffer: *const c_void = std::ptr::null();
    let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        buffer,
        std::ptr::null_mut(),
        list.as_mut_ptr().cast(),
        needed,
        std::ptr::null(),
        std::ptr::null(),
        1,
        &mut block_buffer,
    );
    if status != 0 {
        return Err(format!("the sound couldn't be read from its buffer (status {status})"));
    }
    let count = *(list.as_ptr() as *const u32) as usize;
    let room = (needed - 8) / std::mem::size_of::<AudioBuffer>();
    let buffers = std::slice::from_raw_parts((list.as_ptr() as *const u8).add(8) as *const AudioBuffer, count.min(room));
    let channel = |index: usize| -> &[f32] {
        buffers.get(index).map_or(&[], |part| std::slice::from_raw_parts(part.data as *const f32, part.byte_size as usize / 4))
    };
    let mut out = Vec::with_capacity(frames * usize::from(CHANNELS));
    let non_interleaved = description.format_flags & NON_INTERLEAVED != 0;
    for frame in 0..frames {
        for wanted in 0..usize::from(CHANNELS) {
            let from = wanted.min(source_channels - 1);
            let value = if non_interleaved { channel(from).get(frame) } else { channel(0).get(frame * source_channels + from) };
            out.push(value.copied().unwrap_or(0.0));
        }
    }
    if !block_buffer.is_null() {
        CFRelease(block_buffer);
    }
    Ok(out)
}

extern "C" fn did_output(_this: &Object, _cmd: Sel, _stream: Id, buffer: *const c_void, kind: isize) {
    if kind != AUDIO || buffer.is_null() {
        return;
    }
    // SAFETY: ScreenCaptureKit hands a valid sample buffer of the kind it names, alive for this call.
    let (time, samples) = unsafe { (seconds(CMSampleBufferGetPresentationTimeStamp(buffer)), samples_of(buffer)) };
    let mut shared = SHARED.lock().unwrap();
    let Some(shared) = shared.as_mut() else { return };
    match samples {
        Ok(samples) => {
            let at = ((time - shared.origin_s) * f64::from(RATE)).round() as i64;
            shared.placer.place(at, &samples);
        }
        Err(error) => shared.error = Some(error),
    }
}

fn output_class() -> &'static Class {
    if let Some(class) = Class::get("MCListenOutput") {
        return class;
    }
    let mut declaration = ClassDecl::new("MCListenOutput", class!(NSObject)).expect("a new class name");
    if let Some(protocol) = Protocol::get("SCStreamOutput") {
        declaration.add_protocol(protocol);
    }
    // SAFETY: the method's types match stream:didOutputSampleBuffer:ofType: (object, CMSampleBufferRef, NSInteger).
    unsafe {
        declaration.add_method(sel!(stream:didOutputSampleBuffer:ofType:), did_output as extern "C" fn(&Object, Sel, Id, *const c_void, isize));
    }
    declaration.register()
}

fn description_of(error: Id) -> String {
    if error.is_null() {
        return "no reason given".into();
    }
    // SAFETY: an NSError's localizedDescription is an NSString, whose UTF8String lives while the error does.
    unsafe {
        let text: Id = msg_send![error, localizedDescription];
        let bytes: *const c_char = msg_send![text, UTF8String];
        if bytes.is_null() {
            "no reason given".into()
        } else {
            std::ffi::CStr::from_ptr(bytes).to_string_lossy().into_owned()
        }
    }
}

pub(super) fn capture(sink: &mut Sink) -> io::Result<()> {
    if Class::get("SCStream").is_none() {
        return Err(io::Error::other("listening on macOS needs ScreenCaptureKit (macOS 13 or later)"));
    }
    // SAFETY: Objective-C messages to ScreenCaptureKit with the argument types its headers give; every object made
    // here lives until the end of this function, and the completion blocks are global data.
    unsafe {
        let pool: Id = msg_send![class!(NSAutoreleasePool), new];
        *ANSWERS.lock().unwrap() = Answers::default();
        let handler = global_block(content_handler as *const c_void);
        let _: () = msg_send![class!(SCShareableContent), getShareableContentWithCompletionHandler: &handler];
        let content = wait_for(Duration::from_secs(10), |answers| answers.content.take())
            .ok_or_else(|| io::Error::other("ScreenCaptureKit didn't say what can be recorded in time"))?
            .map_err(|why| io::Error::other(format!("ScreenCaptureKit refused (listening uses the Screen Recording permission): {why}")))? as Id;
        let displays: Id = msg_send![content, displays];
        let count: usize = msg_send![displays, count];
        if count == 0 {
            return Err(io::Error::other("ScreenCaptureKit lists no display to take the sound with"));
        }
        let display: Id = msg_send![displays, objectAtIndex: 0usize];
        let nothing: Id = msg_send![class!(NSArray), array];
        let filter: Id = msg_send![class!(SCContentFilter), alloc];
        let filter: Id = msg_send![filter, initWithDisplay: display excludingWindows: nothing];
        let configuration: Id = msg_send![class!(SCStreamConfiguration), new];
        let _: () = msg_send![configuration, setCapturesAudio: YES];
        let _: () = msg_send![configuration, setExcludesCurrentProcessAudio: YES];
        let _: () = msg_send![configuration, setSampleRate: RATE as isize];
        let _: () = msg_send![configuration, setChannelCount: CHANNELS as isize];
        // The picture isn't wanted: as small as allowed.
        let _: () = msg_send![configuration, setWidth: 2usize];
        let _: () = msg_send![configuration, setHeight: 2usize];
        let stream: Id = msg_send![class!(SCStream), alloc];
        let null: Id = std::ptr::null_mut();
        let stream: Id = msg_send![stream, initWithFilter: filter configuration: configuration delegate: null];
        let output: Id = msg_send![output_class(), new];
        let queue = dispatch_queue_create(c"master-control.listen".as_ptr(), std::ptr::null());
        for kind in [0isize, AUDIO] {
            // The picture goes to the same output, which drops it; without one ScreenCaptureKit complains of each frame.
            let mut error: Id = std::ptr::null_mut();
            let added: BOOL = msg_send![stream, addStreamOutput: output type: kind sampleHandlerQueue: queue error: &mut error];
            if added != YES {
                return Err(io::Error::other(format!("ScreenCaptureKit wouldn't take the sound's output: {}", description_of(error))));
            }
        }

        *SHARED.lock().unwrap() = Some(Shared { origin_s: host_now(), placer: Placer::new(CHANNELS), error: None });
        let started = global_block(started_handler as *const c_void);
        let _: () = msg_send![stream, startCaptureWithCompletionHandler: &started];
        match wait_for(Duration::from_secs(10), |answers| answers.started.take()) {
            Some(None) => {}
            Some(Some(why)) => return Err(io::Error::other(format!("ScreenCaptureKit didn't start recording: {why}"))),
            None => return Err(io::Error::other("ScreenCaptureKit didn't start recording in time")),
        }
        sink.started(RATE, CHANNELS)?;

        let outcome = loop {
            std::thread::sleep(Duration::from_millis(20));
            let (waiting, error) = {
                let mut shared = SHARED.lock().unwrap();
                let shared = shared.as_mut().expect("set above");
                let quiet_until = ((host_now() - shared.origin_s - LATE_S).max(0.0) * f64::from(RATE)) as u64;
                shared.placer.pad_to(quiet_until);
                (shared.placer.take(), shared.error.take())
            };
            if let Some(error) = error {
                break Err(io::Error::other(error));
            }
            if let Err(error) = sink.write(&waiting) {
                break Err(error);
            }
            if sink.should_stop() {
                break Ok(());
            }
        };
        let stopped = global_block(stopped_handler as *const c_void);
        let _: () = msg_send![stream, stopCaptureWithCompletionHandler: &stopped];
        let _ = wait_for(Duration::from_secs(5), |answers| answers.stopped.then_some(()));
        // What came in up to the stop is kept.
        let rest = SHARED.lock().unwrap().take().map(|mut shared| shared.placer.take()).unwrap_or_default();
        if outcome.is_ok() {
            sink.write(&rest)?;
        }
        let _: () = msg_send![pool, drain];
        outcome
    }
}
