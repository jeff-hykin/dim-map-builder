//! AV1 decoding for cameras recorded as video, and the OBU reading that finding a keyframe needs.
//!
//! AV1 has no annex-B start codes to scan for: a temporal unit is a run of OBUs that each carry their own size, so
//! walking one is exact rather than a search. Its sequence header OBU plays the part H.264's SPS/PPS play -- only it
//! says how big the picture is, and no decoder can start without one.
//!
//! The decoder is rav1d, the pure-Rust port of dav1d. That matters here for the same reason OpenH264 is compiled in
//! rather than shelled out to: the map builder ships to robots as one static musl binary, and dav1d itself is C built
//! by meson, which does not go quietly into such a build. rav1d's hand-written assembly is behind the `av1-asm`
//! feature because on x86 building it shells out to nasm; the robot's aarch64 build assembles it with the C compiler
//! already in use, so turn it on there.

use anyhow::{anyhow, bail, ensure, Result};
use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::headers::{DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420};
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib::{dav1d_close, dav1d_data_create, dav1d_default_settings, dav1d_get_picture, dav1d_open, dav1d_picture_unref, dav1d_send_data};
use std::mem::MaybeUninit;
use std::ptr::NonNull;

/// OBU types read here (AV1 spec 6.2.2); every other type is stepped over by its size field.
const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_FRAME_HEADER: u8 = 3;
const OBU_FRAME: u8 = 6;
/// `frame_type` in an uncompressed frame header (spec 6.8.2).
const KEY_FRAME: u8 = 0;

/// One LEB128 value and the bytes it took (spec 4.10.5), or None past the end.
fn leb128(data: &[u8]) -> Option<(usize, usize)> {
    let mut value = 0usize;
    for index in 0..8 {
        let byte = *data.get(index)?;
        value |= ((byte & 0x7f) as usize) << (index * 7);
        if byte & 0x80 == 0 {
            return Some((value, index + 1));
        }
    }
    None
}

/// The OBUs in a temporal unit as (type, payload), stopping wherever the bytes stop making sense rather than
/// erroring: this is used to classify a recorded frame, and a damaged frame should read as "no keyframe here", not
/// end the recording.
fn obus(data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut obus = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let header = data[at];
        // the forbidden bit must be clear, and without a size field an OBU runs to the end of the buffer, so a unit
        // holding more than one could not be split at all: either way this is not a low-overhead OBU stream
        if header & 0x80 != 0 || (header >> 1) & 1 != 1 {
            return obus;
        }
        let kind = (header >> 3) & 0xf;
        at += 1 + usize::from((header >> 2) & 1 == 1);
        let Some(rest) = data.get(at..) else { return obus };
        let Some((size, read)) = leb128(rest) else { return obus };
        at += read;
        let Some(payload) = data.get(at..at + size) else { return obus };
        at += size;
        obus.push((kind, payload));
    }
    obus
}

/// A shown key frame: a frame OBU whose uncompressed header says `show_existing_frame` 0, `frame_type` KEY_FRAME and
/// `show_frame` 1. With `reduced_still_picture_header` the sequence header leaves a frame OBU no header bits at all
/// and the frame is a shown key frame by definition (spec 5.9.2).
pub fn is_keyframe(data: &[u8]) -> bool {
    let obus = obus(data);
    let reduced = obus
        .iter()
        .find(|(kind, _)| *kind == OBU_SEQUENCE_HEADER)
        // seq_profile f(3), still_picture f(1), reduced_still_picture_header f(1)
        .and_then(|(_, payload)| payload.first())
        .is_some_and(|first| (first >> 3) & 1 == 1);
    obus.iter().any(|(kind, payload)| {
        if !matches!(*kind, OBU_FRAME | OBU_FRAME_HEADER) {
            return false;
        }
        if reduced {
            return true;
        }
        payload.first().is_some_and(|bits| bits >> 7 == 0 && (bits >> 5) & 3 == KEY_FRAME && (bits >> 4) & 1 == 1)
    })
}

/// A decoded picture's luma plane, rows packed to `width` (the decoder's own are padded to its stride).
pub struct Luma {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Turns a recorded AV1 camera's temporal units into pictures, in order.
///
/// One per stream: the rav1d context carries the reference frames, which is what makes a frame stored as the
/// difference from earlier ones decodable at all.
pub struct Frames {
    context: Option<Dav1dContext>,
}

impl Frames {
    pub fn new() -> Result<Self> {
        let mut settings = MaybeUninit::<Dav1dSettings>::uninit();
        // SAFETY: dav1d_default_settings writes a whole Dav1dSettings through the pointer and reads nothing.
        unsafe { dav1d_default_settings(NonNull::new(settings.as_mut_ptr()).expect("a stack slot is never null")) };
        // SAFETY: dav1d_default_settings initialised every field.
        let mut settings = unsafe { settings.assume_init() };
        // one picture out for each unit in: frames are read in recording order and wanted in that order, so a frame
        // delay would only mean holding pictures back and flushing for them later
        settings.n_threads = 1;
        settings.max_frame_delay = 1;
        let mut context: Option<Dav1dContext> = None;
        // SAFETY: both pointers are to live locals, which is all dav1d_open asks.
        let result = unsafe { dav1d_open(NonNull::new(&mut context), NonNull::new(&mut settings)) };
        ensure!(result.0 == 0 && context.is_some(), "starting the AV1 decoder (rav1d error {})", result.0);
        Ok(Frames { context })
    }

    /// The picture for this temporal unit, or None when the decoder cannot produce one yet -- which is normal at the
    /// start of a recording, where the first frames reference pictures the recording does not contain.
    pub fn push(&mut self, data: &[u8]) -> Result<Option<Luma>> {
        let mut buffer = Dav1dData::default();
        // SAFETY: `buffer` is a live local; the call either fills it and returns its data, or returns null.
        let into = unsafe { dav1d_data_create(NonNull::new(&mut buffer), data.len()) };
        ensure!(!into.is_null(), "the AV1 decoder could not take {} bytes", data.len());
        // SAFETY: dav1d_data_create just returned `data.len()` writable bytes at `into`, which `data` cannot overlap.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), into, data.len()) };
        // SAFETY: the context is open and `buffer` is a live local holding the data just written.
        let sent = unsafe { dav1d_send_data(self.context, NonNull::new(&mut buffer)) };
        if sent.0 != 0 {
            // a frame the decoder will not take is not the end of a recording; the next keyframe recovers
            return Ok(None);
        }
        let mut picture = Dav1dPicture::default();
        // SAFETY: the context is open and `picture` is a live, zeroed local, as dav1d_get_picture wants.
        let got = unsafe { dav1d_get_picture(self.context, NonNull::new(&mut picture)) };
        // owned from here, so every return below unreferences it
        let picture = Picture(picture);
        if got.0 != 0 {
            return Ok(None);
        }
        let parameters = &picture.0.p;
        ensure!(
            parameters.layout == DAV1D_PIXEL_LAYOUT_I420 || parameters.layout == DAV1D_PIXEL_LAYOUT_I400,
            "the AV1 stream is not 4:2:0 or monochrome (layout {})",
            parameters.layout
        );
        ensure!(parameters.bpc == 8, "the AV1 stream is {}-bit, and marker detection reads 8-bit luma", parameters.bpc);
        let (width, height) = (usize::try_from(parameters.w)?, usize::try_from(parameters.h)?);
        let stride = usize::try_from(picture.0.stride[0])?;
        let plane = picture.0.data[0].ok_or_else(|| anyhow!("the AV1 decoder returned no luma plane"))?;
        // SAFETY: rav1d owns the plane until `picture` is dropped, and it holds `stride * height` bytes.
        let luma = unsafe { std::slice::from_raw_parts(plane.as_ptr() as *const u8, stride * height) };
        let mut data = Vec::with_capacity(width * height);
        for row in 0..height {
            let from = row * stride;
            if from + width > luma.len() {
                bail!("the AV1 decoder returned a short picture ({width}x{height}, stride {stride})");
            }
            data.extend_from_slice(&luma[from..from + width]);
        }
        Ok(Some(Luma { width: width as u32, height: height as u32, data }))
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        // SAFETY: `context` is the one dav1d_open filled, and this runs once.
        unsafe { dav1d_close(NonNull::new(&mut self.context)) };
    }
}

/// A decoded picture, unreferenced on drop: it owns the planes read out of it.
struct Picture(Dav1dPicture);

impl Drop for Picture {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a live local, either the picture dav1d_get_picture filled or the zeroed one it left.
        unsafe { dav1d_picture_unref(NonNull::new(&mut self.0)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporal delimiter, a sequence header and a frame OBU, each with its size field: the shape SVT-AV1 writes.
    fn unit(frame_header_byte: u8, with_sequence_header: bool) -> Vec<u8> {
        let mut data = vec![0x12, 0x00]; // temporal delimiter (type 2), size 0
        if with_sequence_header {
            // sequence header (type 1), size 1, reduced_still_picture_header clear
            data.extend_from_slice(&[0x0a, 0x01, 0x00]);
        }
        // frame (type 6), size 1
        data.extend_from_slice(&[0x32, 0x01, frame_header_byte]);
        data
    }

    #[test]
    fn a_shown_key_frame_is_told_from_a_predicted_one() {
        // show_existing_frame 0, frame_type 0 (KEY_FRAME), show_frame 1
        assert!(is_keyframe(&unit(0b0001_0000, true)));
        // frame_type 1 (INTER_FRAME), shown
        assert!(!is_keyframe(&unit(0b0011_0000, true)));
        // a key frame that is not shown
        assert!(!is_keyframe(&unit(0b0000_0000, true)));
        // show_existing_frame set: this unit shows a frame decoded earlier, so it starts nothing
        assert!(!is_keyframe(&unit(0b1001_0000, true)));
        // no sequence header is still a key frame by its own header; whether it decodes is the decoder's business
        assert!(is_keyframe(&unit(0b0001_0000, false)));
    }

    #[test]
    fn reduced_still_picture_makes_the_frame_a_key_frame() {
        // sequence header with reduced_still_picture_header set, then a frame OBU with no header bits to read
        let data = vec![0x12, 0x00, 0x0a, 0x01, 0b0000_1000, 0x32, 0x01, 0xff];
        assert!(is_keyframe(&data));
    }

    #[test]
    fn annex_b_and_rubbish_read_as_no_keyframe() {
        // H.264's 00 00 00 01 start code: the first byte has no size field
        assert!(!is_keyframe(&[0, 0, 0, 1, 0x65, 0x88]));
        assert!(!is_keyframe(&[]));
        // a size field that runs past the end
        assert!(!is_keyframe(&[0x32, 0x40, 1, 2]));
    }

    #[test]
    fn the_obu_walk_stops_at_the_forbidden_bit() {
        assert!(obus(&[0x80]).is_empty());
    }
}
