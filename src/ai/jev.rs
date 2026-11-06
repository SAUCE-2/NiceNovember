//! Minimal client for TypeSafe AI's Jev (https://docs.typesafe.ai): typed, probabilistic answers.
//!
//! You send `state` (text/JSON, optionally referencing images by placeholder) and a map of named
//! questions; Jev returns one typed answer per question.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const MODEL: &str = "jev-latest";

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
#[allow(dead_code)] // Choice/Score/Box are kept for future questions
pub enum Question {
    /// True/false question; answer is a probability in 0..=1.
    Noul {
        instructions: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    /// Pick one option; `criteria` maps option name -> optional description.
    Choice {
        instructions: String,
        criteria: BTreeMap<String, Option<String>>,
    },
    /// Place the input on an ordered rubric (index 0 = first level).
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
    /// Locate one thing in an image (Jev 1.14+). Always returns a box, even if the thing is
    /// absent, so pair it with a Noul asking whether it's there.
    Box { instructions: String },
}

#[derive(Clone, Debug, Serialize)]
pub struct NoulCriteria {
    #[serde(rename = "true")]
    pub yes: String,
    #[serde(rename = "false")]
    pub no: String,
}

pub fn noul(instructions: impl Into<String>, criteria: Option<(&str, &str)>) -> Question {
    Question::Noul {
        instructions: instructions.into(),
        criteria: criteria.map(|(y, n)| NoulCriteria {
            yes: y.into(),
            no: n.into(),
        }),
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
#[allow(dead_code)] // Choice/Score/Box are kept for future questions
pub enum Answer {
    Noul {
        noul: f32,
    },
    Choice {
        choice: String,
        confidence: f32,
        probabilities: HashMap<String, f32>,
    },
    Score {
        score: f32,
        confidence: f32,
        #[serde(default)]
        probabilities: HashMap<String, f32>,
    },
    Box {
        #[serde(rename = "box")]
        bounds: BoxBounds,
        confidence: f32,
    },
    #[serde(other)]
    Other,
}

/// Normalized 0..=1 edges from the image's top-left corner; multiply by width/height for pixels.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct BoxBounds {
    pub top: f32,
    pub bottom: f32,
    pub left: f32,
    pub right: f32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Answers {
    pub answers: HashMap<String, Answer>,
}

impl Answers {
    pub fn noul(&self, id: &str) -> Result<f32> {
        match self.answers.get(id) {
            Some(Answer::Noul { noul }) => Ok(*noul),
            other => Err(anyhow!("expected noul answer for {id}, got {other:?}")),
        }
    }
}

/// An image to attach, referenced from `state`/instructions by its placeholder string.
#[derive(Clone)]
pub struct Image {
    pub placeholder: String,
    /// Baseline JPEG bytes already normalized with [`normalize_image`].
    pub jpeg: Vec<u8>,
}

#[derive(Clone)]
pub struct Jev {
    http: reqwest::Client,
    key: String,
}

impl Jev {
    pub fn new(key: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("reqwest client");
        Self { http, key }
    }

    pub async fn ask(
        &self,
        state: Value,
        questions: impl IntoIterator<Item = (&str, Question)>,
        images: Vec<Image>,
    ) -> Result<Answers> {
        let body = request_body(state, questions, &images);
        let mut delay = Duration::from_millis(500);
        for attempt in 0.. {
            let resp = self.request(&body, &images)?.send().await;
            let resp = resp.context("jev request failed")?;
            let status = resp.status();
            if status.is_success() {
                return resp.json().await.context("bad jev response");
            }
            let text = resp.text().await.unwrap_or_default();
            if !retryable(status.as_u16(), attempt) {
                bail!(
                    "jev returned {status}: {}",
                    text.chars().take(300).collect::<String>()
                );
            }
            tokio::time::sleep(delay).await;
            delay *= 2;
        }
        unreachable!("the loop only ends by returning")
    }

    fn request(&self, body: &Value, images: &[Image]) -> Result<reqwest::RequestBuilder> {
        let req = self.http.post(ENDPOINT).bearer_auth(&self.key);
        if images.is_empty() {
            return Ok(req.json(body));
        }
        Ok(req
            .header("TypeSafe-Danger-Zone-Images", "true")
            .multipart(multipart(body, images)?))
    }
}

/// 429 rate limited / 529 overloaded are worth retrying, three times.
pub fn retryable(status: u16, attempt: u32) -> bool {
    matches!(status, 429 | 529) && attempt < 3
}

fn multipart(body: &Value, images: &[Image]) -> Result<Form> {
    let mut form = Form::new().part(
        "request",
        Part::text(body.to_string()).mime_str("application/json")?,
    );
    for (i, img) in images.iter().enumerate() {
        form = form.part(
            part_name(i),
            Part::bytes(img.jpeg.clone())
                .file_name(format!("{}.jpg", part_name(i)))
                .mime_str("image/jpeg")?,
        );
    }
    Ok(form)
}

fn part_name(i: usize) -> String {
    format!("img{i}")
}

fn request_body<'a>(
    state: Value,
    questions: impl IntoIterator<Item = (&'a str, Question)>,
    images: &[Image],
) -> Value {
    let questions: BTreeMap<&str, Question> = questions.into_iter().collect();
    let mut body = json!({ "model": MODEL, "state": state, "questions": questions });
    if !images.is_empty() {
        let assets: BTreeMap<&str, Value> = images
            .iter()
            .enumerate()
            .map(|(i, img)| {
                (
                    img.placeholder.as_str(),
                    json!({ "type": "image", "part": part_name(i) }),
                )
            })
            .collect();
        body["assets"] = json!(assets);
    }
    body
}

/// Jev only accepts PNG/JPEG stills with each side in 280..=1568 px, and rejects (never resizes)
/// anything else. Returns the size to resize to; may distort extreme aspect ratios slightly.
pub fn target_size(w: u32, h: u32) -> (u32, u32) {
    const MIN: f64 = 280.0;
    const MAX: f64 = 1568.0;
    let (w, h) = (w.max(1) as f64, h.max(1) as f64);
    let mut s = 1.0f64.min(MAX / w.max(h));
    if w.min(h) * s < MIN {
        s = MIN / w.min(h);
    }
    let fit = |v: f64| (v * s).round().clamp(MIN, MAX) as u32;
    (fit(w), fit(h))
}

/// Decode any common format (first frame of GIF/WebP), rotate upright per EXIF (Jev ignores
/// EXIF orientation), fit to Jev's size limits, and re-encode as baseline JPEG (much smaller
/// than PNG for photos; transparency is flattened).
pub fn normalize_image(bytes: &[u8]) -> Result<Vec<u8>> {
    use image::{DynamicImage, ImageDecoder, ImageReader};
    let mut decoder = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()?
        .into_decoder()
        .context("unsupported image")?;
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    let (w, h) = target_size(img.width(), img.height());
    let img = if (w, h) == (img.width(), img.height()) {
        img
    } else {
        img.resize_exact(w, h, image::imageops::FilterType::Triangle)
    };
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
        .encode_image(&img.to_rgb8())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_shape() {
        let body = request_body(
            json!("hi"),
            [("x", noul("Is it festive?", Some(("yes", "no"))))],
            &[Image {
                placeholder: "ph-1".into(),
                jpeg: vec![],
            }],
        );
        assert_eq!(
            body,
            json!({
                "model": "jev-latest",
                "state": "hi",
                "questions": { "x": { "type": "noul", "instructions": "Is it festive?",
                                      "criteria": { "true": "yes", "false": "no" } } },
                "assets": { "ph-1": { "type": "image", "part": "img0" } }
            })
        );
        let no_crit = request_body(json!("hi"), [("y", noul("q", None))], &[]);
        assert_eq!(
            no_crit["questions"]["y"],
            json!({ "type": "noul", "instructions": "q" })
        );
        assert!(no_crit.get("assets").is_none());
    }

    #[test]
    fn retries_only_overload_three_times() {
        assert!(retryable(429, 0));
        assert!(retryable(529, 2));
        assert!(!retryable(529, 3));
        assert!(!retryable(500, 0));
        assert!(!retryable(401, 0));
    }

    #[test]
    fn parses_answers() {
        let a: Answers = serde_json::from_value(json!({
            "model": "jev-1.13.0",
            "answers": {
                "n": { "type": "noul", "noul": 0.99 },
                "c": { "type": "choice", "choice": "a", "confidence": 1.0, "probabilities": { "a": 1.0 } },
                "s": { "type": "score", "score": 1.43, "confidence": 0.35,
                       "legend": { "0": "x" }, "probabilities": { "0": 0.5 } },
                "b": { "type": "box", "box": { "top": 0.25, "bottom": 0.82, "left": 0.44, "right": 0.84 },
                       "confidence": 0.92 },
                "x": { "type": "future_thing" }
            },
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        }))
        .unwrap();
        assert_eq!(a.noul("n").unwrap(), 0.99);
        assert!(a.noul("c").is_err());
        assert!(matches!(a.answers["b"], Answer::Box { confidence, .. } if confidence == 0.92));
        assert!(matches!(a.answers["x"], Answer::Other));
    }

    #[test]
    fn image_sizes_fit_limits() {
        assert_eq!(target_size(800, 600), (800, 600));
        assert_eq!(target_size(4000, 3000), (1568, 1176));
        assert_eq!(target_size(100, 100), (280, 280));
        assert_eq!(target_size(128, 64), (560, 280));
        for (w, h) in [(1, 10000), (10000, 1), (5000, 200), (37, 37)] {
            let (a, b) = target_size(w, h);
            assert!(
                (280..=1568).contains(&a) && (280..=1568).contains(&b),
                "{w}x{h} -> {a}x{b}"
            );
        }
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;
    use image::{DynamicImage, ImageFormat, RgbaImage};

    #[test]
    fn normalizes_small_transparent_png_to_jpeg() {
        let mut src = Vec::new();
        DynamicImage::ImageRgba8(RgbaImage::new(64, 32))
            .write_to(&mut std::io::Cursor::new(&mut src), ImageFormat::Png)
            .unwrap();
        let out = normalize_image(&src).unwrap();
        let back = image::load_from_memory_with_format(&out, ImageFormat::Jpeg).unwrap();
        assert_eq!((back.width(), back.height()), (560, 280));
    }
}

/// Live API check (sends synthetic data only):
/// `./dev.sh cargo test live_jev -- --ignored --nocapture` with JEV_API_KEY in `.env`.
#[cfg(test)]
mod live {
    use super::*;
    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};

    #[tokio::test]
    #[ignore]
    async fn live_jev() {
        let jev = Jev::new(std::env::var("JEV_API_KEY").expect("JEV_API_KEY"));

        let text = jev
            .ask(
                json!("shut up, nobody asked you, you absolute clown"),
                [(
                    "hostile",
                    noul("Is the author being hostile towards someone?", None),
                )],
                vec![],
            )
            .await
            .unwrap();
        println!("text: hostile={:?}", text.noul("hostile"));

        // White canvas with a red square in the top-left quadrant.
        let mut img = RgbImage::from_pixel(600, 400, Rgb([255, 255, 255]));
        for x in 60..240 {
            for y in 40..160 {
                img.put_pixel(x, y, Rgb([220, 20, 20]));
            }
        }
        let mut png = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();
        let jpeg = normalize_image(&png).unwrap();
        let ph = "3f2a9c1e-test-photo";
        let a = jev
            .ask(
                json!({ "photo": ph }),
                [
                    ("has_red", noul("Is there a red square in `photo`?", None)),
                    ("has_tree", noul("Is there a tree in `photo`?", None)),
                    (
                        "where",
                        Question::Box {
                            instructions: "the red square in `photo`".into(),
                        },
                    ),
                ],
                vec![Image {
                    placeholder: ph.into(),
                    jpeg,
                }],
            )
            .await
            .unwrap();
        println!("image: answers={:?}", a.answers);
    }
}
