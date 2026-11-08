//! Cameras recorded as video (`foxglove_msgs.CompressedVideo`: H.264 access units or AV1 temporal units) rather than
//! as stills.
//!
//! A video frame is not a picture on its own. Only a keyframe decodes by itself; every frame after it is stored as
//! the difference from the ones before. The decoder is therefore stateful: access units go in in recording order and
//! pictures come out as it has them.
//!
//! The decoders are compiled into this binary: OpenH264 from source for H.264, and rav1d (the pure-Rust dav1d port)
//! for AV1. Nothing is spawned and there is nothing to install -- the map builder ships to robots as one static musl
//! binary, and an external ffmpeg would undo that. H.265 is not decoded here: libde265 would have to be found on the
//! machine, which that build cannot do.
//!
//! Frames come out as `mono8` -- the decoder's own luma plane, which is what marker detection reads, with no
//! conversion to colour and back.
use crate::av1;
use crate::lcm::{Header, Image, Reader};
use anyhow::{bail, Context, Result};
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;

pub const COMPRESSED_VIDEO_TYPE: &str = "foxglove_msgs.CompressedVideo";
/// From the generated dimos-lcm python type, and checked against a recording's first message.
pub const COMPRESSED_VIDEO_FINGERPRINT: [u8; 8] = [0x22, 0x17, 0xb4, 0x6e, 0xef, 0xbf, 0x7b, 0x05];

/// Does this dimos type / ROS 2 schema name name a camera recorded as video?
pub fn is_video_type(type_name: &str) -> bool {
    let normalized = type_name.replace("/msg/", ".").replace('/', ".");
    normalized == COMPRESSED_VIDEO_TYPE || normalized.ends_with(".CompressedVideo") || normalized == "CompressedVideo"
}

/// One message of such a stream: the access unit for a single frame, and what it takes to decode it.
#[derive(Debug, Clone)]
pub struct Unit {
    pub ts: f64,
    pub frame_id: String,
    /// "h264", "h265", "av1", ... as the publisher named it
    pub format: String,
    pub data: Vec<u8>,
}

impl Unit {
    pub fn h265(&self) -> bool {
        let format = self.format.to_lowercase();
        format.contains("265") || format.contains("hevc")
    }

    /// AV1. ffmpeg and foxglove both also write the fourcc spelling.
    pub fn av1(&self) -> bool {
        let format = self.format.to_lowercase();
        format.contains("av1") || format.contains("av01")
    }

    /// Whether a decoder could start at this frame, read the way this unit's own format stores it.
    pub fn is_keyframe(&self) -> bool {
        if self.av1() {
            av1::is_keyframe(&self.data)
        } else {
            annex_b_keyframe(&self.data, self.h265())
        }
    }
}

/// `foxglove_msgs.CompressedVideo` as dimos puts it on the wire: data_length, timestamp, frame_id, data, format.
pub fn decode_lcm(payload: &[u8]) -> Result<Unit> {
    let mut reader = Reader::new(payload);
    reader.expect_fingerprint(&COMPRESSED_VIDEO_FINGERPRINT)?;
    let length = reader.length()?;
    let sec = reader.i32()?;
    let nanosec = reader.i32()?;
    let frame_id = reader.string()?;
    let data = reader.take(length)?.to_vec();
    let format = reader.string()?;
    Ok(Unit { ts: sec as f64 + nanosec as f64 * 1e-9, frame_id, format, data })
}

pub fn decode_unit(encoding: &str, payload: &[u8]) -> Result<Unit> {
    if encoding == "cdr" {
        crate::cdr::decode_compressed_video(payload)
    } else {
        decode_lcm(payload)
    }
}

/// The NAL types in an annex-B access unit: each unit starts with 00 00 01 or 00 00 00 01. H.264 keeps the type in
/// the low 5 bits of the byte that follows, H.265 in bits 1-6.
fn nal_types(bytes: &[u8], h265: bool) -> Vec<u8> {
    let mut types = Vec::new();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] != 0 || bytes[i + 1] != 0 {
            i += 1;
            continue;
        }
        let at = if bytes[i + 2] == 1 {
            i + 3
        } else if bytes[i + 2] == 0 && bytes[i + 3] == 1 {
            i + 4
        } else {
            i += 1;
            continue;
        };
        if at >= bytes.len() {
            break;
        }
        types.push(if h265 { (bytes[at] >> 1) & 0x3f } else { bytes[at] & 0x1f });
        i = at;
    }
    types
}

/// H.264: an IDR slice is 5. H.265: the IRAP types (BLA/IDR/CRA) are 16-23. AV1 has no NAL units at all, so it goes
/// through [`av1::is_keyframe`] instead -- see [`Unit::is_keyframe`], which picks by format.
pub fn annex_b_keyframe(bytes: &[u8], h265: bool) -> bool {
    nal_types(bytes, h265).iter().any(|t| if h265 { (16..=23).contains(t) } else { *t == 5 })
}

/// The decoder a stream ended up with. Which one is decided by the first frame's `format` rather than up front,
/// because that is the only place a recording says what it holds -- and a stream is one codec throughout, so a frame
/// in another one is a recording that disagrees with itself rather than something to switch decoders for.
enum Decoding {
    H264(Decoder),
    Av1(av1::Frames),
}

impl Decoding {
    fn for_unit(unit: &Unit) -> Result<Self> {
        if unit.h265() {
            bail!("{} is H.265, which this binary has no decoder for (it links OpenH264 and rav1d)", unit.format);
        }
        if unit.av1() {
            Ok(Decoding::Av1(av1::Frames::new()?))
        } else {
            Ok(Decoding::H264(Decoder::new().context("starting the H.264 decoder")?))
        }
    }

    /// Whether `unit` is the codec this decoder was made for, so a stream that changes codec is reported instead of
    /// being fed to a decoder that cannot read it.
    fn takes(&self, unit: &Unit) -> bool {
        matches!((self, unit.av1()), (Decoding::Av1(_), true) | (Decoding::H264(_), false))
    }
}

/// Turns a recorded camera's access units into pictures, in order.
///
/// One of these per stream: it carries the decoder's state, which is what makes a frame that depends on earlier ones
/// decodable at all.
pub struct Frames {
    /// None until the first frame says which codec the stream is
    decoding: Option<Decoding>,
    /// frames handed in that the decoder had no picture for: the count is how a stream that never decodes is told
    /// from one that merely starts mid-group
    pending: u64,
    decoded: u64,
}

impl Frames {
    pub fn new() -> Result<Self> {
        Ok(Frames { decoding: None, pending: 0, decoded: 0 })
    }

    /// The picture for this access unit, or None when the decoder cannot produce one yet.
    ///
    /// None is normal at the start of a recording: a recorder begins wherever the user pressed record, so the first
    /// frames reference pictures the recording does not contain and nothing can bring them back.
    pub fn push(&mut self, unit: &Unit) -> Result<Option<Image>> {
        let decoding = match &mut self.decoding {
            Some(decoding) => decoding,
            None => self.decoding.insert(Decoding::for_unit(unit)?),
        };
        if !decoding.takes(unit) {
            bail!("the stream changed codec mid-way (a frame says {}), which no single decoder can follow", unit.format);
        }
        let picture = match decoding {
            Decoding::Av1(frames) => frames.push(&unit.data)?.map(|luma| (luma.width, luma.height, luma.data)),
            Decoding::H264(decoder) => match decoder.decode(&unit.data) {
                // a damaged frame is not the end of a recording; the next keyframe recovers
                Err(_) => None,
                Ok(decoded) => decoded
                    .map(|yuv| {
                        let (width, height) = yuv.dimensions();
                        let (stride, _, _) = yuv.strides();
                        let luma = yuv.y();
                        // the decoder's rows are padded to its own stride; an Image's are not
                        let mut data = Vec::with_capacity(width * height);
                        for row in 0..height {
                            let from = row * stride;
                            if from + width > luma.len() {
                                bail!("the decoder returned a short picture ({width}x{height}, stride {stride})");
                            }
                            data.extend_from_slice(&luma[from..from + width]);
                        }
                        Ok((width as u32, height as u32, data))
                    })
                    .transpose()?,
            },
        };
        let Some((width, height, data)) = picture else {
            self.pending += 1;
            return Ok(None);
        };
        self.decoded += 1;
        Ok(Some(Image {
            header: Header { stamp_sec: unit.ts.floor() as i32, stamp_nsec: ((unit.ts - unit.ts.floor()) * 1e9).round() as i32, frame_id: unit.frame_id.clone() },
            height,
            width,
            encoding: "mono8".into(),
            step: width,
            data,
        }))
    }

    /// How many access units produced a picture, and how many did not.
    pub fn counts(&self) -> (u64, u64) {
        (self.decoded, self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_types_are_recognised() {
        assert!(is_video_type("foxglove_msgs.CompressedVideo"));
        assert!(is_video_type("foxglove_msgs/msg/CompressedVideo"));
        assert!(is_video_type("CompressedVideo"));
        assert!(!is_video_type("sensor_msgs.CompressedImage"));
        assert!(!is_video_type("sensor_msgs.Image"));
    }

    #[test]
    fn keyframes_are_told_from_the_rest() {
        // an IDR slice (type 5) after a start code, and a non-IDR one (type 1)
        let key = [0, 0, 0, 1, 0x65, 0x88, 0x84];
        let inter = [0, 0, 0, 1, 0x41, 0x9a, 0x02];
        assert!(annex_b_keyframe(&key, false));
        assert!(!annex_b_keyframe(&inter, false));
        // an H.265 IRAP (type 19 << 1) is a keyframe only when read as H.265
        let h265_key = [0, 0, 0, 1, 19 << 1, 0x01];
        assert!(annex_b_keyframe(&h265_key, true));
        assert!(!annex_b_keyframe(&h265_key, false));
    }

    #[test]
    fn a_recorded_message_decodes_to_its_access_unit() {
        // the first bytes of a real m20 front_camera message: fingerprint, data_length, timestamp, frame_id
        let mut payload = Vec::new();
        payload.extend_from_slice(&COMPRESSED_VIDEO_FINGERPRINT);
        payload.extend_from_slice(&3i32.to_be_bytes()); // data_length
        payload.extend_from_slice(&1_794_171_226i32.to_be_bytes()); // sec
        payload.extend_from_slice(&49_762_470i32.to_be_bytes()); // nanosec
        payload.extend_from_slice(&4i32.to_be_bytes()); // "cam" + NUL
        payload.extend_from_slice(b"cam\0");
        payload.extend_from_slice(&[9, 8, 7]);
        payload.extend_from_slice(&5i32.to_be_bytes());
        payload.extend_from_slice(b"h264\0");
        let unit = decode_lcm(&payload).expect("decodes");
        assert_eq!(unit.frame_id, "cam");
        assert_eq!(unit.format, "h264");
        assert_eq!(unit.data, vec![9, 8, 7]);
        assert!((unit.ts - 1_794_171_226.049_762_47).abs() < 1e-6, "{}", unit.ts);
        assert!(!unit.h265());
    }

    #[test]
    fn another_types_bytes_are_refused() {
        let mut payload = vec![0u8; 32];
        payload[..8].copy_from_slice(&crate::lcm::IMAGE_FINGERPRINT);
        assert!(decode_lcm(&payload).is_err());
    }
}

#[cfg(test)]
mod decode_tests {
    use super::*;
    use std::process::Command;

    /// A real H.264 stream, split into one access unit per frame (keyframe every `gop`).
    ///
    /// ffmpeg makes the fixture; it is not what decodes it. Encoding here only has to produce a genuine stream for
    /// the decoder under test to meet, rather than bytes shaped like one.
    fn encoded_units(frames: usize, gop: usize) -> Option<Vec<Vec<u8>>> {
        let has_ffmpeg = Command::new("ffmpeg").arg("-version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success());
        if !has_ffmpeg {
            return None;
        }
        let dir = std::env::temp_dir().join(format!("dimos-video-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("clip.h264");
        let made = Command::new("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i", &format!("testsrc=size=64x48:rate=10:duration={}", frames as f64 / 10.0)])
            .args(["-c:v", "libx264", "-g", &gop.to_string(), "-bf", "0", "-pix_fmt", "yuv420p", "-f", "h264"])
            .arg(&path)
            .arg("-y")
            .output()
            .ok()?;
        if !made.status.success() {
            std::fs::remove_dir_all(&dir).ok();
            return None;
        }
        let bytes = std::fs::read(&path).ok()?;
        std::fs::remove_dir_all(&dir).ok();

        // cut at each picture start: a VCL unit (types 1 and 5), carrying the parameter sets that precede it
        let mut marks: Vec<(usize, bool)> = Vec::new();
        let mut i = 0;
        while i + 3 < bytes.len() {
            if bytes[i] != 0 || bytes[i + 1] != 0 {
                i += 1;
                continue;
            }
            let (at, start) = if bytes[i + 2] == 1 {
                (i + 3, i)
            } else if bytes[i + 2] == 0 && bytes[i + 3] == 1 {
                (i + 4, i)
            } else {
                i += 1;
                continue;
            };
            let kind = bytes[at] & 0x1f;
            if matches!(kind, 1 | 5 | 7 | 8) {
                marks.push((start, matches!(kind, 1 | 5)));
            }
            i = at;
        }
        let mut units = Vec::new();
        for (index, (start, vcl)) in marks.iter().enumerate() {
            if !vcl {
                continue;
            }
            let mut from = *start;
            for back in (0..index).rev() {
                if marks[back].1 {
                    break;
                }
                from = marks[back].0;
            }
            let next = marks[index + 1..].iter().position(|m| m.1).map(|n| index + 1 + n);
            let mut to = bytes.len();
            if let Some(next) = next {
                to = marks[next].0;
                for back in (0..next).rev() {
                    if marks[back].1 {
                        break;
                    }
                    to = marks[back].0;
                }
            }
            units.push(bytes[from..to].to_vec());
        }
        Some(units)
    }

    fn unit(data: Vec<u8>, ts: f64) -> Unit {
        Unit { ts, frame_id: "camera".into(), format: "h264".into(), data }
    }

    fn av1_unit(data: Vec<u8>, ts: f64) -> Unit {
        Unit { ts, frame_id: "camera".into(), format: "av1".into(), data }
    }

    /// A real AV1 stream (low-overhead OBU), split into one temporal unit per frame.
    ///
    /// Splitting is exact here, unlike the H.264 fixture above: every temporal unit opens with a temporal delimiter
    /// OBU, and every OBU carries its own size, so there is nothing to search for.
    fn av1_units(frames: usize, gop: usize) -> Option<Vec<Vec<u8>>> {
        let encoders = Command::new("ffmpeg").args(["-hide_banner", "-encoders"]).output().ok()?;
        if !String::from_utf8_lossy(&encoders.stdout).contains("libsvtav1") {
            return None;
        }
        let dir = std::env::temp_dir().join(format!("dimos-av1-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("clip.obu");
        let made = Command::new("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i", &format!("testsrc=size=64x48:rate=10:duration={}", frames as f64 / 10.0)])
            .args(["-c:v", "libsvtav1", "-g", &gop.to_string(), "-svtav1-params", "lp=1", "-pix_fmt", "yuv420p", "-f", "obu"])
            .arg(&path)
            .arg("-y")
            .output()
            .ok()?;
        if !made.status.success() {
            std::fs::remove_dir_all(&dir).ok();
            return None;
        }
        let bytes = std::fs::read(&path).ok()?;
        std::fs::remove_dir_all(&dir).ok();
        let mut units: Vec<Vec<u8>> = Vec::new();
        let mut start = None;
        let mut at = 0;
        while at < bytes.len() {
            let header = bytes[at];
            let kind = (header >> 3) & 0xf;
            let mut cursor = at + 1 + usize::from((header >> 2) & 1 == 1);
            let mut size = 0usize;
            for shift in (0..56).step_by(7) {
                let byte = *bytes.get(cursor)?;
                cursor += 1;
                size |= ((byte & 0x7f) as usize) << shift;
                if byte & 0x80 == 0 {
                    break;
                }
            }
            // a temporal delimiter opens a temporal unit
            if kind == 2 {
                if let Some(from) = start {
                    units.push(bytes[from..at].to_vec());
                }
                start = Some(at);
            }
            at = cursor + size;
        }
        if let Some(from) = start {
            units.push(bytes[from..].to_vec());
        }
        Some(units)
    }

    #[test]
    fn a_real_av1_stream_decodes_to_gray_frames() {
        let Some(units) = av1_units(20, 10) else {
            eprintln!("no ffmpeg with libsvtav1 to make the fixture: skipped");
            return;
        };
        assert!(units.len() >= 11, "expected at least 11 temporal units, got {}", units.len());
        assert!(av1_unit(units[0].clone(), 0.0).is_keyframe(), "the stream must open on a keyframe");
        assert!(!av1_unit(units[1].clone(), 0.0).is_keyframe(), "the frame after a keyframe is predicted");
        let mut frames = Frames::new().unwrap();
        let mut pictures = Vec::new();
        for (index, data) in units.iter().enumerate() {
            if let Some(image) = frames.push(&av1_unit(data.clone(), 100.0 + index as f64 * 0.1)).unwrap() {
                pictures.push(image);
            }
        }
        let (decoded, _) = frames.counts();
        assert_eq!(decoded as usize, pictures.len());
        assert!(pictures.len() >= 10, "only {} pictures out of {} units", pictures.len(), units.len());
        let first = &pictures[0];
        assert_eq!((first.width, first.height, first.encoding.as_str()), (64, 48, "mono8"));
        assert_eq!(first.data.len(), 64 * 48, "a mono8 row is the picture's width, with no stride padding");
        assert_eq!(first.step, 64);
        // the pictures carry their own unit's time, not the newest one's
        assert_eq!(first.header.stamp_sec, 100);
        // and they are different pictures, not one keyframe handed back repeatedly
        assert_ne!(pictures[0].data, pictures[pictures.len() - 1].data, "every frame decoded to the same picture");
    }

    #[test]
    fn an_av1_stream_is_refused_by_the_h264_path_and_the_other_way_round() {
        let Some(av1) = av1_units(4, 10) else {
            eprintln!("no ffmpeg with libsvtav1 to make the fixture: skipped");
            return;
        };
        // a stream that starts AV1 and then claims H.264 is a recording disagreeing with itself
        let mut frames = Frames::new().unwrap();
        frames.push(&av1_unit(av1[0].clone(), 0.0)).unwrap();
        let error = frames.push(&unit(av1[1].clone(), 0.1)).unwrap_err().to_string();
        assert!(error.contains("changed codec"), "{error}");
    }

    #[test]
    fn a_real_stream_decodes_to_gray_frames_at_the_recording_s_own_times() {
        let Some(units) = encoded_units(20, 10) else {
            eprintln!("no ffmpeg to make the fixture: skipped");
            return;
        };
        assert!(annex_b_keyframe(&units[0], false), "the clip must open on a keyframe");
        let mut frames = Frames::new().expect("decoder");
        let mut got: Vec<(f64, Image)> = Vec::new();
        for (index, data) in units.iter().enumerate() {
            let ts = 1700.0 + index as f64 * 0.1;
            if let Some(image) = frames.push(&unit(data.clone(), ts)).expect("decodes") {
                got.push((ts, image));
            }
        }
        assert!(got.len() >= units.len() - 1, "expected a picture for nearly every frame, got {} of {}", got.len(), units.len());
        for (ts, image) in &got {
            assert_eq!(image.encoding, "mono8");
            assert_eq!((image.width, image.height), (64, 48));
            // the rows are packed to the width, not left at the decoder's stride
            assert_eq!(image.data.len(), 64 * 48);
            assert_eq!(image.step, image.width);
            assert_eq!(image.header.frame_id, "camera");
            // the time is the recording's own, not anything the decoder invented
            assert!((image.header.ts() - ts).abs() < 1e-3, "{} vs {ts}", image.header.ts());
        }
        // and they are different pictures: a moving test pattern must not come back as one frame repeated
        assert_ne!(got[0].1.data, got[got.len() - 1].1.data);
    }

    #[test]
    fn frames_before_the_first_keyframe_are_not_passed_off_as_pictures() {
        // A recording is cut wherever the user pressed record, so it can open mid-group. Those frames reference
        // pictures the recording does not hold; the decoder must say it has nothing rather than invent one.
        let Some(units) = encoded_units(20, 10) else {
            eprintln!("no ffmpeg to make the fixture: skipped");
            return;
        };
        let mut frames = Frames::new().expect("decoder");
        let mut got = 0;
        for data in &units[1..5] {
            if frames.push(&unit(data.clone(), 1700.0)).expect("no hard error").is_some() {
                got += 1;
            }
        }
        assert_eq!(got, 0, "decoded {got} pictures from frames with no keyframe before them");
        // and the next keyframe recovers: the stream is not left broken by that
        let recovered = units[10..].iter().filter_map(|data| frames.push(&unit(data.clone(), 1700.0)).ok().flatten()).count();
        assert!(recovered > 0, "the decoder never recovered at the next keyframe");
    }

    #[test]
    fn h265_is_refused_rather_than_decoded_as_h264() {
        let mut frames = Frames::new().expect("decoder");
        let mut unit = unit(vec![0, 0, 0, 1, 19 << 1, 0x01], 1700.0);
        unit.format = "h265".into();
        let error = frames.push(&unit).expect_err("H.265 must not be read as H.264");
        assert!(error.to_string().contains("H.265"), "{error}");
    }
}
