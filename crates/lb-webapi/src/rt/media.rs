//! Media type support: `HTMLMediaElement.canPlayType`,
//! `MediaSource.isTypeSupported`, `MediaRecorder.isTypeSupported` and
//! `navigator.mediaCapabilities` (`decodingInfo`, `encodingInfo`).
//!
//! The tables are branded Chrome 153 on the desktop (proprietary codecs:
//! H.264, AAC, MP3), captured on Linux from the local Chrome for Testing
//! (`tests/fixtures/os/linux_x11_cft153.json`, `media`). What depends on
//! the OS comes from the persona's `codecs()`: HEVC plays only with
//! hardware decode (Windows and macOS with a hardware GPU; `""` on Linux),
//! and `powerEfficient` follows hardware decode. Windows and macOS answers
//! are the M7 surface reference, not captured.

use lb_js::{Arg, CallCtx, HostError, ValueKind};
use lb_persona::Codecs;

use super::{Dict, Runtime, Window, getter, method, need_args, this_node};
use crate::registry::{ImplKind, Registry};

/// What `canPlayType` answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    No,
    Maybe,
    Probably,
}

impl Answer {
    fn as_str(self) -> &'static str {
        match self {
            Answer::No => "",
            Answer::Maybe => "maybe",
            Answer::Probably => "probably",
        }
    }
}

/// Containers Chrome knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Container {
    Mp4,
    WebM,
    Ogg,
    Mpeg,
    Aac,
    Flac,
    Wav,
    Matroska,
    ThreeGp,
    Hls,
    Mp2t,
}

/// Codec families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Codec {
    H264,
    Hevc,
    Vp8,
    Vp9,
    Av1,
    DolbyVision,
    Aac,
    Mp3,
    Opus,
    Vorbis,
    Flac,
    Pcm,
    Ac3,
    Theora,
}

impl Codec {
    fn is_video(self) -> bool {
        matches!(
            self,
            Codec::H264
                | Codec::Hevc
                | Codec::Vp8
                | Codec::Vp9
                | Codec::Av1
                | Codec::DolbyVision
                | Codec::Theora
        )
    }
}

/// `type/subtype` and the `codecs` list of a MIME string, lower-cased.
/// `None` when the string is not a MIME type.
fn parse_mime(s: &str) -> Option<(String, Option<Vec<String>>)> {
    let mut parts = s.split(';');
    let essence = parts.next()?.trim().to_ascii_lowercase();
    let (ty, sub) = essence.split_once('/')?;
    let token = |t: &str| {
        !t.is_empty()
            && t.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    };
    if !token(ty) || !token(sub) {
        return None;
    }
    let mut codecs = None;
    for p in parts {
        let Some((k, v)) = p.split_once('=') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case("codecs") {
            let v = v.trim().trim_matches('"');
            codecs = Some(
                v.split(',')
                    .map(|c| c.trim().to_owned())
                    .filter(|c| !c.is_empty())
                    .collect(),
            );
        }
    }
    Some((essence, codecs))
}

/// The container of a MIME essence, whether it is audio-only, and what
/// `canPlayType` says without codecs.
fn container(essence: &str) -> Option<(Container, bool, Answer)> {
    use Answer::{Maybe, No, Probably};
    use Container as C;
    Some(match essence {
        "video/mp4" | "video/x-m4v" => (C::Mp4, false, Maybe),
        "audio/mp4" | "audio/x-m4a" => (C::Mp4, true, Maybe),
        "video/webm" => (C::WebM, false, Maybe),
        "audio/webm" => (C::WebM, true, Maybe),
        "video/ogg" | "application/ogg" => (C::Ogg, false, Maybe),
        "audio/ogg" => (C::Ogg, true, Maybe),
        "audio/mpeg" | "audio/mp3" => (C::Mpeg, true, Probably),
        "audio/aac" => (C::Aac, true, Probably),
        "audio/flac" => (C::Flac, true, Probably),
        "audio/wav" | "audio/x-wav" | "audio/wave" => (C::Wav, true, Maybe),
        "video/x-matroska" => (C::Matroska, false, Maybe),
        "video/3gpp" => (C::ThreeGp, false, Maybe),
        // Captured: native HLS answers "maybe".
        "application/vnd.apple.mpegurl" | "application/x-mpegurl" | "audio/mpegurl" => {
            (C::Hls, false, Maybe)
        }
        "video/mp2t" => (C::Mp2t, false, No),
        _ => return None,
    })
}

/// A codec string (`avc1.42E01E`, `vp09.00.10.08`, `mp4a.40.2`, `opus`).
fn codec(s: &str) -> Option<Codec> {
    let l = s.to_ascii_lowercase();
    let (head, rest) = l.split_once('.').unwrap_or((l.as_str(), ""));
    let hex = |t: &str, n: usize| t.len() == n && t.bytes().all(|b| b.is_ascii_hexdigit());
    Some(match head {
        "avc1" | "avc3" => {
            if !rest.is_empty() && !hex(rest, 6) {
                return None;
            }
            Codec::H264
        }
        "hvc1" | "hev1" => Codec::Hevc,
        "dvh1" | "dvhe" | "dva1" | "dvav" => Codec::DolbyVision,
        "vp8" | "vp8.0" => Codec::Vp8,
        "vp9" | "vp9.0" => Codec::Vp9,
        "vp09" => {
            let f: Vec<&str> = rest.split('.').collect();
            if f.len() < 3 || f.iter().any(|x| x.parse::<u32>().is_err()) {
                return None;
            }
            Codec::Vp9
        }
        "av01" => {
            if rest.split('.').count() < 3 {
                return None;
            }
            Codec::Av1
        }
        "mp4a" => match rest {
            "40.2" | "40.02" | "40.5" | "40.05" | "40.29" | "40.42" | "40" | "67" => Codec::Aac,
            "69" | "6b" | "40.34" => Codec::Mp3,
            "a5" | "a6" => Codec::Ac3,
            _ => return None,
        },
        "ac-3" | "ec-3" => Codec::Ac3,
        "mp3" => Codec::Mp3,
        "opus" => Codec::Opus,
        "vorbis" => Codec::Vorbis,
        "flac" | "flac.0" => Codec::Flac,
        "1" | "pcm" => Codec::Pcm,
        "theora" => Codec::Theora,
        _ => return None,
    })
}

/// A codec plays in a container (`canPlayType`, `mse` for
/// `MediaSource.isTypeSupported`).
fn plays(cont: Container, c: Codec, audio_only: bool, codecs: &Codecs, mse: bool) -> bool {
    use Codec as K;
    use Container as C;
    if audio_only && c.is_video() {
        return false;
    }
    match c {
        // Never in Chrome on the desktop.
        K::DolbyVision | K::Ac3 | K::Theora => return false,
        K::Hevc if !codecs.hevc => return false,
        _ => {}
    }
    match cont {
        C::Mp4 => matches!(
            c,
            K::H264 | K::Hevc | K::Vp9 | K::Av1 | K::Aac | K::Mp3 | K::Opus | K::Flac
        ),
        C::WebM => matches!(c, K::Vp8 | K::Vp9 | K::Av1 | K::Opus | K::Vorbis),
        C::Ogg => !mse && matches!(c, K::Opus | K::Vorbis | K::Flac),
        C::Mpeg => c == K::Mp3,
        C::Aac => c == K::Aac,
        C::Flac => !mse && c == K::Flac,
        C::Wav => !mse && c == K::Pcm,
        C::Matroska => {
            !mse && matches!(c, K::H264 | K::Vp8 | K::Vp9 | K::Av1 | K::Opus | K::Vorbis)
        }
        C::ThreeGp => !mse && matches!(c, K::H264 | K::Aac),
        C::Hls => false,
        C::Mp2t => mse && matches!(c, K::H264 | K::Aac | K::Mp3),
    }
}

fn persona_codecs(w: &Window) -> Codecs {
    w.profile.as_deref().map_or(
        Codecs {
            hevc: false,
            h264_power_efficient: false,
            av1_hw: false,
        },
        lb_persona::PersonaProfile::codecs,
    )
}

/// `canPlayType(type)`.
fn can_play_type(s: &str, codecs: &Codecs) -> Answer {
    let Some((essence, list)) = parse_mime(s) else {
        return Answer::No;
    };
    let Some((cont, audio_only, bare)) = container(&essence) else {
        return Answer::No;
    };
    match list {
        None => bare,
        Some(list) if list.is_empty() => bare,
        Some(list) => {
            if cont == Container::Hls {
                return Answer::Maybe;
            }
            let ok = list
                .iter()
                .all(|c| codec(c).is_some_and(|k| plays(cont, k, audio_only, codecs, false)));
            if ok { Answer::Probably } else { Answer::No }
        }
    }
}

/// `MediaSource.isTypeSupported(type)`.
fn mse_supported(s: &str, codecs: &Codecs) -> bool {
    let Some((essence, list)) = parse_mime(s) else {
        return false;
    };
    // Byte streams MSE has, by exact type (captured: `audio/x-m4a` and
    // `audio/flac` are false).
    if !matches!(
        essence.as_str(),
        "video/mp4"
            | "audio/mp4"
            | "video/webm"
            | "audio/webm"
            | "audio/mpeg"
            | "audio/aac"
            | "video/mp2t"
    ) {
        return false;
    }
    let Some((cont, audio_only, _)) = container(&essence) else {
        return false;
    };
    match list {
        None => true,
        Some(list) => list
            .iter()
            .all(|c| codec(c).is_some_and(|k| plays(cont, k, audio_only, codecs, true))),
    }
}

/// `MediaRecorder.isTypeSupported(type)` (captured on Linux; HEVC and AAC
/// recording are off).
fn recorder_supported(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    let Some((essence, list)) = parse_mime(s) else {
        return false;
    };
    let ok: &[&str] = match essence.as_str() {
        "video/webm" | "video/x-matroska" => &[
            "vp8", "vp9", "vp8.0", "vp9.0", "av1", "av01", "h264", "avc1", "opus", "pcm",
        ],
        "video/mp4" => &["avc1", "avc3", "vp9", "vp09", "av01", "av1", "opus"],
        "audio/webm" => &["opus", "pcm"],
        "audio/mp4" => &["opus"],
        _ => return false,
    };
    match list {
        None => true,
        Some(list) => list.iter().all(|c| {
            let l = c.to_ascii_lowercase();
            let head = l.split('.').next().unwrap_or("");
            ok.contains(&l.as_str())
                || (matches!(head, "avc1" | "avc3" | "vp09" | "av01") && ok.contains(&head))
        }),
    }
}

/// `decodingInfo`'s `powerEfficient` for a supported codec.
fn power_efficient(k: Codec, codecs: &Codecs) -> bool {
    match k {
        Codec::H264 | Codec::Vp9 => codecs.h264_power_efficient,
        Codec::Hevc => codecs.hevc,
        Codec::Av1 => codecs.av1_hw,
        _ => false,
    }
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// One `video` or `audio` member of a media configuration, read and
/// checked like Blink's dictionary conversion.
struct TrackConfig {
    content_type: String,
    json: String,
}

fn read_track(
    cx: &mut CallCtx<'_, '_, '_>,
    d: &Dict,
    key: &str,
    op: &str,
    dict_name: &str,
) -> Result<Option<TrackConfig>, HostError> {
    let Some(h) = d.get(cx, key)? else {
        return Ok(None);
    };
    let kind = cx.handle_kind(h);
    if !matches!(kind, ValueKind::Object | ValueKind::Function) {
        cx.release(h);
        return Err(HostError::type_error(format!(
            "Failed to execute '{op}' on 'MediaCapabilities': Failed to read the '{key}' property from '{dict_name}': The provided value is not of type '{}'.",
            if key == "video" {
                "VideoConfiguration"
            } else {
                "AudioConfiguration"
            }
        )));
    }
    let t = Dict(Some(h));
    let missing = |m: &str, ty: &str| {
        HostError::type_error(format!(
            "Failed to execute '{op}' on 'MediaCapabilities': Failed to read the '{key}' property from '{dict_name}': Failed to read the '{m}' property from '{ty}': Required member is undefined."
        ))
    };
    let r = if key == "video" {
        let ty = "VideoConfiguration";
        let ct = t.string(cx, "contentType")?;
        let width = t.number(cx, "width")?;
        let height = t.number(cx, "height")?;
        let bitrate = t.number(cx, "bitrate")?;
        let framerate = t.number(cx, "framerate")?;
        let ct = ct.ok_or_else(|| missing("contentType", ty))?;
        let width = width.ok_or_else(|| missing("width", ty))?;
        let height = height.ok_or_else(|| missing("height", ty))?;
        let bitrate = bitrate.ok_or_else(|| missing("bitrate", ty))?;
        let framerate = framerate.ok_or_else(|| missing("framerate", ty))?;
        TrackConfig {
            json: format!(
                "{{\"bitrate\":{},\"contentType\":{},\"framerate\":{},\"height\":{},\"width\":{}}}",
                bitrate.trunc(),
                json_str(&ct),
                framerate,
                height.trunc(),
                width.trunc()
            ),
            content_type: ct,
        }
    } else {
        let ct = t
            .string(cx, "contentType")?
            .ok_or_else(|| missing("contentType", "AudioConfiguration"))?;
        TrackConfig {
            json: format!("{{\"contentType\":{}}}", json_str(&ct)),
            content_type: ct,
        }
    };
    t.release(cx);
    Ok(Some(r))
}

/// Chrome's answer for one track: (supported, powerEfficient), or a
/// TypeError for a content type that is not a valid media MIME type.
fn track_support(
    t: &TrackConfig,
    video: bool,
    mse: bool,
    codecs: &Codecs,
    op: &str,
) -> Result<(bool, bool), HostError> {
    let bad = || {
        HostError::type_error(format!(
            "Failed to execute '{op}' on 'MediaCapabilities': The provided value '{}' is not a valid {} MIME type.",
            t.content_type,
            if video { "video" } else { "audio" }
        ))
    };
    let (essence, list) = parse_mime(&t.content_type).ok_or_else(bad)?;
    let prefix = if video { "video/" } else { "audio/" };
    if !essence.starts_with(prefix) {
        return Err(bad());
    }
    let list = list.unwrap_or_default();
    // Exactly one codec per track (spec: "valid media MIME type" with a
    // single codec for video); none is unsupported, not an error.
    if list.len() != 1 {
        return Ok((false, false));
    }
    let Some((cont, audio_only, _)) = container(&essence) else {
        return Ok((false, false));
    };
    let Some(k) = codec(&list[0]) else {
        return Ok((false, false));
    };
    if video != k.is_video() {
        return Ok((false, false));
    }
    let ok = if mse {
        mse_supported(&t.content_type, codecs)
    } else {
        plays(cont, k, audio_only, codecs, false)
    };
    Ok((ok, ok && (!video || power_efficient(k, codecs))))
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    method(reg, rt, "HTMLMediaElement", "canPlayType", |cx, w| {
        this_node(cx)?;
        need_args(cx, 1, "canPlayType", "HTMLMediaElement")?;
        let t = cx.arg_string(0)?;
        cx.ret(can_play_type(&t, &persona_codecs(w)).as_str())
    });
    reg.register(
        "MediaSource",
        "isTypeSupported",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, w| {
            need_args(cx, 1, "isTypeSupported", "MediaSource")?;
            let t = cx.arg_string(0)?;
            cx.ret(mse_supported(&t, &persona_codecs(w)))
        }),
    );
    reg.register(
        "MediaRecorder",
        "isTypeSupported",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, _w| {
            need_args(cx, 1, "isTypeSupported", "MediaRecorder")?;
            let t = cx.arg_string(0)?;
            cx.ret(recorder_supported(&t))
        }),
    );
    getter(reg, rt, "Navigator", "mediaCapabilities", |cx, w| {
        super::this_obj(cx)?;
        cx.ret(super::navigator::single_nav(
            w,
            "nav:mediacapabilities",
            super::navigator::NavObj::MediaCapabilities,
        ))
    });
    for (op, dict_name, types) in [
        (
            "decodingInfo",
            "MediaDecodingConfiguration",
            &["file", "media-source", "webrtc"][..],
        ),
        (
            "encodingInfo",
            "MediaEncodingConfiguration",
            &["record", "webrtc"][..],
        ),
    ] {
        method(reg, rt, "MediaCapabilities", op, move |cx, w| {
            super::navigator::this_is(cx, w, |o| {
                matches!(o, super::navigator::NavObj::MediaCapabilities)
            })?;
            let r = cx.ret_new_promise()?;
            match media_info(cx, w, op, dict_name, types) {
                Ok(json) => cx.resolve_promise(r, Arg::Json(json)),
                Err(e) => cx.reject_promise(r, e),
            }
        });
    }
}

/// `decodingInfo` / `encodingInfo`: the result dictionary as JSON.
fn media_info(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    op: &str,
    dict_name: &str,
    types: &[&str],
) -> Result<String, HostError> {
    if cx.is_empty() {
        return Err(HostError::type_error(format!(
            "Failed to execute '{op}' on 'MediaCapabilities': 1 argument required, but only 0 present."
        )));
    }
    let k = cx.arg_kind(0);
    if !matches!(
        k,
        ValueKind::Object | ValueKind::Function | ValueKind::Undefined | ValueKind::Null
    ) {
        return Err(HostError::type_error(format!(
            "Failed to execute '{op}' on 'MediaCapabilities': The provided value is not of type '{dict_name}'."
        )));
    }
    let d = Dict::arg(cx, 0);
    // Dictionary members are read in alphabetical order (audio, ...,
    // type, video), like Blink's generated conversion.
    let audio = read_track(cx, &d, "audio", op, dict_name);
    let ty = d.string(cx, "type");
    let video = read_track(cx, &d, "video", op, dict_name);
    d.release(cx);
    let (audio, ty, video) = (audio?, ty?, video?);
    let ty = ty.ok_or_else(|| {
        HostError::type_error(format!(
            "Failed to execute '{op}' on 'MediaCapabilities': Failed to read the 'type' property from '{dict_name}': Required member is undefined."
        ))
    })?;
    if !types.contains(&ty.as_str()) {
        let enum_name = if op == "decodingInfo" {
            "MediaDecodingType"
        } else {
            "MediaEncodingType"
        };
        return Err(HostError::type_error(format!(
            "Failed to execute '{op}' on 'MediaCapabilities': Failed to read the 'type' property from '{dict_name}': The provided value '{ty}' is not a valid enum value of type {enum_name}."
        )));
    }
    if audio.is_none() && video.is_none() {
        return Err(HostError::type_error(format!(
            "Failed to execute '{op}' on 'MediaCapabilities': The configuration dictionary has neither |video| nor |audio| specified."
        )));
    }
    let codecs = persona_codecs(w);
    let mse = ty == "media-source";
    let mut supported = true;
    let mut efficient = true;
    if let Some(v) = &video {
        let (s, e) = track_support(v, true, mse, &codecs, op)?;
        supported &= s;
        efficient &= e;
    }
    if let Some(a) = &audio {
        let (s, e) = track_support(a, false, mse, &codecs, op)?;
        supported &= s;
        efficient &= e;
    }
    let mut cfg = String::from("{");
    if let Some(a) = &audio {
        cfg.push_str(&format!("\"audio\":{},", a.json));
    }
    cfg.push_str(&format!("\"type\":{}", json_str(&ty)));
    if let Some(v) = &video {
        cfg.push_str(&format!(",\"video\":{}", v.json));
    }
    cfg.push('}');
    let tail = if op == "decodingInfo" {
        format!(",\"configuration\":{cfg},\"keySystemAccess\":null")
    } else {
        format!(",\"configuration\":{cfg}")
    };
    Ok(format!(
        "{{\"powerEfficient\":{},\"smooth\":{supported},\"supported\":{supported}{tail}}}",
        supported && efficient
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX: Codecs = Codecs {
        hevc: false,
        h264_power_efficient: false,
        av1_hw: false,
    };
    const WIN: Codecs = Codecs {
        hevc: true,
        h264_power_efficient: true,
        av1_hw: true,
    };

    /// Every `canPlayType` and `isTypeSupported` answer of the Linux
    /// capture.
    #[test]
    fn linux_capture_matches() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/os/linux_x11_cft153.json"
        );
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let m = &v["result"]["media"];
        for (t, want) in m["canPlayType"].as_object().unwrap() {
            assert_eq!(
                can_play_type(t, &LINUX).as_str(),
                want.as_str().unwrap(),
                "canPlayType({t})"
            );
        }
        for (t, want) in m["mse"].as_object().unwrap() {
            assert_eq!(
                mse_supported(t, &LINUX),
                want.as_bool().unwrap(),
                "MediaSource.isTypeSupported({t})"
            );
        }
        for (t, want) in m["recorder"].as_object().unwrap() {
            assert_eq!(
                recorder_supported(t),
                want.as_bool().unwrap(),
                "MediaRecorder.isTypeSupported({t})"
            );
        }
    }

    #[test]
    fn hevc_follows_hardware_decode() {
        let t = "video/mp4; codecs=\"hvc1.1.6.L93.B0\"";
        assert_eq!(can_play_type(t, &WIN), Answer::Probably);
        assert_eq!(can_play_type(t, &LINUX), Answer::No);
        assert!(mse_supported(t, &WIN));
        assert_eq!(can_play_type("video/mp4", &WIN), Answer::Maybe);
        assert_eq!(can_play_type("bogus", &WIN), Answer::No);
        assert_eq!(
            can_play_type("audio/mp4; codecs=\"avc1.42E01E\"", &WIN),
            Answer::No
        );
    }
}
