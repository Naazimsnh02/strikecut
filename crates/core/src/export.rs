//! Rendering a cut list with ffmpeg.
//!
//! We use one `select`/`aselect` pass instead of `trim` + `concat` per span: it streams the input once with
//! constant memory, which matters for hour-long recordings with hundreds of cuts.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::Stdio;

use anyhow::{bail, Context, Result};

use crate::edit::{output_duration, Span};
use crate::media::{self, command, ffmpeg};

#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// x264 constant rate factor for video containers (lower is higher quality).
    pub crf: u8,
    pub preset: String,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            crf: 20,
            preset: "medium".into(),
        }
    }
}

/// `gte(t,a)*lt(t,b)+...`, the expression `select`/`aselect` use to keep half-open spans `[a, b)`.
/// `shift` moves both edges, used to keep frame-exact boundaries robust to timestamp rounding.
///
/// ffmpeg's expression parser recurses once per `+` and gives up past ~100 terms, so the sum is built as a
/// balanced tree: `(A+B)+(C+D)` nests log2(n) deep instead of n.
fn select_expr(spans: &[Span], shift: f64) -> String {
    fn sum(terms: &[String]) -> String {
        match terms {
            [one] => one.clone(),
            _ if terms.len() <= 8 => terms.join("+"),
            _ => {
                let (l, r) = terms.split_at(terms.len() / 2);
                format!("({})+({})", sum(l), sum(r))
            }
        }
    }
    let terms: Vec<String> = spans
        .iter()
        .map(|s| format!("gte(t,{:.6})*lt(t,{:.6})", s.start + shift, s.end + shift))
        .collect();
    sum(&terms)
}

/// Parse an ffprobe rate like `30000/1001` or `25`.
fn parse_rate(rate: &str) -> Option<f64> {
    let r = match rate.split_once('/') {
        Some((n, d)) => n.trim().parse::<f64>().ok()? / d.trim().parse::<f64>().ok()?,
        None => rate.trim().parse().ok()?,
    };
    (r.is_finite() && r > 0.0).then_some(r)
}

/// Snap span edges outward to frame boundaries so the video keeps exactly as much time as the audio.
/// Without this, every span can gain or lose a frame and A/V drifts apart over hundreds of cuts.
/// Snapping outward (never inward) means a cut can't clip the edge of a kept word.
fn snap_to_frames(spans: &[Span], fps: f64) -> Vec<Span> {
    const EPS: f64 = 1e-6;
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for s in spans {
        let snapped = Span {
            start: (s.start * fps + EPS).floor() / fps,
            end: (s.end * fps - EPS).ceil() / fps,
        };
        match out.last_mut() {
            Some(last) if snapped.start <= last.end + EPS => last.end = last.end.max(snapped.end),
            _ if snapped.end > snapped.start => out.push(snapped),
            _ => {}
        }
    }
    out
}

/// Build the `-filter_complex` graph. With video, spans are snapped to frames and video is forced to
/// constant frame rate, so renumbering timestamps by frame count keeps it in sync with the audio
/// (screen recordings are often VFR).
pub fn filter_graph(spans: &[Span], video_fps: Option<&str>) -> String {
    let fps = video_fps.and_then(|r| parse_rate(r).map(|f| (r, f)));
    let spans = match fps {
        Some((_, f)) => snap_to_frames(spans, f),
        None => spans.to_vec(),
    };
    let audio = format!("[0:a:0]asetnsamples=n=64:p=0,aselect='{}',asetpts=N/SR/TB[outa]", select_expr(&spans, 0.0));
    match fps {
        // Frame k sits at exactly k/fps; shifting edges back a quarter frame makes [a, b) pick exactly
        // frames a*fps..b*fps even when timestamps carry rounding error.
        Some((rate, f)) => format!(
            "[0:v:0]fps={rate},select='{}',setpts=N/FRAME_RATE/TB[outv];{audio}",
            select_expr(&spans, -0.25 / f)
        ),
        None => audio,
    }
}

/// Render `spans` of `input` to `output`. `on_progress` receives the fraction complete (0.0..=1.0).
pub fn export(
    input: &Path,
    spans: &[Span],
    output: &Path,
    opts: &ExportOptions,
    mut on_progress: impl FnMut(f64),
) -> Result<()> {
    if spans.is_empty() {
        bail!("nothing to export: every word is deleted");
    }
    let info = media::probe(input)?;
    if !info.has_audio {
        bail!("{} has no audio track", input.display());
    }
    let fps = if info.has_video { Some(video_fps(input)?) } else { None };
    let graph = filter_graph(spans, fps.as_deref());
    let total = output_duration(spans);

    let mut cmd = command(ffmpeg());
    cmd.args(["-y", "-v", "error", "-nostats", "-progress", "pipe:1", "-i"]).arg(input);

    // Long cut lists can exceed the OS command-line limit, so pass big graphs via a file.
    let _graph_file;
    if graph.len() > 8000 {
        let f = tempfile::Builder::new().suffix(".ffgraph").tempfile()?;
        std::fs::write(f.path(), &graph)?;
        cmd.arg("-/filter_complex").arg(f.path());
        _graph_file = Some(f);
    } else {
        cmd.arg("-filter_complex").arg(&graph);
        _graph_file = None;
    }

    if fps.is_some() {
        cmd.args(["-map", "[outv]"]);
    }
    cmd.args(["-map", "[outa]"]);

    let ext = output.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if fps.is_some() && matches!(ext.as_str(), "mp4" | "mov" | "m4v" | "mkv") {
        cmd.args(["-c:v", "libx264", "-preset", &opts.preset, "-crf", &opts.crf.to_string(), "-pix_fmt", "yuv420p"]);
        cmd.args(["-c:a", "aac", "-b:a", "192k"]);
        if ext != "mkv" {
            cmd.args(["-movflags", "+faststart"]);
        }
    }
    cmd.arg(output);

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to run ffmpeg; is it installed and on PATH?")?;

    let mut stderr = child.stderr.take().expect("piped");
    let stderr_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    for line in BufReader::new(child.stdout.take().expect("piped")).lines() {
        let line = line?;
        if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok()) {
            if total > 0.0 {
                on_progress((us / 1_000_000.0 / total).clamp(0.0, 1.0));
            }
        }
    }

    let status = child.wait()?;
    let err = stderr_reader.join().unwrap_or_default();
    if !status.success() {
        bail!("ffmpeg failed ({status}):\n{}", err.trim());
    }
    on_progress(1.0);
    Ok(())
}

fn video_fps(input: &Path) -> Result<String> {
    let out = command(media::ffprobe())
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=avg_frame_rate", "-of", "csv=p=0"])
        .arg(input)
        .output()?;
    let rate = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // ffprobe reports "0/0" when it can't tell; fall back to 30.
    Ok(match rate.as_str() {
        "" | "0/0" => "30".into(),
        r => r.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_frame_snapped_select_graph() {
        // At 10 fps, spans snap outward: 1.53 -> 1.6 and 2.26 -> 2.2. Video edges shift back a quarter frame.
        let spans = [Span { start: 0.0, end: 1.53 }, Span { start: 2.26, end: 3.0 }];
        assert_eq!(
            filter_graph(&spans, Some("10/1")),
            "[0:v:0]fps=10/1,select='gte(t,-0.025000)*lt(t,1.575000)+gte(t,2.175000)*lt(t,2.975000)',setpts=N/FRAME_RATE/TB[outv];\
             [0:a:0]asetnsamples=n=64:p=0,aselect='gte(t,0.000000)*lt(t,1.600000)+gte(t,2.200000)*lt(t,3.000000)',asetpts=N/SR/TB[outa]"
        );
        let audio_only = filter_graph(&spans, None);
        assert!(!audio_only.contains("outv") && audio_only.contains("lt(t,1.530000)"));
    }

    #[test]
    fn snapping_merges_spans_that_meet() {
        let spans = [Span { start: 0.0, end: 1.01 }, Span { start: 1.09, end: 2.0 }];
        assert_eq!(snap_to_frames(&spans, 10.0), [Span { start: 0.0, end: 2.0 }]);
    }

    #[test]
    fn select_expr_nests_logarithmically() {
        let spans: Vec<Span> = (0..1000).map(|i| Span { start: i as f64, end: i as f64 + 0.5 }).collect();
        let expr = select_expr(&spans, 0.0);
        let mut depth = 0i32;
        let mut max = 0;
        for c in expr.chars() {
            depth += match c { '(' => 1, ')' => -1, _ => 0 };
            max = max.max(depth);
        }
        assert!(max < 20, "nesting depth {max}");
        assert_eq!(expr.matches("gte(").count(), 1000);
    }

    #[test]
    fn parses_rates() {
        assert_eq!(parse_rate("25"), Some(25.0));
        assert!((parse_rate("30000/1001").unwrap() - 29.97).abs() < 0.01);
        assert_eq!(parse_rate("0/0"), None);
    }
}
