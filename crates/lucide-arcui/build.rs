use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

#[derive(Clone, Copy)]
struct Segment {
    start: (f32, f32),
    end: (f32, f32),
}

fn icon_id_for(stem: &str) -> String {
    let mut name = String::with_capacity(stem.len() + 5);
    name.push_str("icon_");
    for ch in stem.chars() {
        match ch {
            '-' => name.push('_'),
            'a'..='z' | '0'..='9' | '_' => name.push(ch),
            _ => name.push('_'),
        }
    }
    name
}

fn read_icon_paths(icons_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(icons_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension() == Some(OsStr::new("svg")) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn attr_f32(node: roxmltree::Node<'_, '_>, name: &str, default: f32) -> Result<f32> {
    Ok(node
        .attribute(name)
        .map(|value| value.parse::<f32>())
        .transpose()?
        .unwrap_or(default))
}

fn parse_points(points: &str) -> Result<Vec<(f32, f32)>> {
    let mut values = Vec::new();
    for part in points.split(|ch: char| ch == ',' || ch.is_ascii_whitespace()) {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        values.push(trimmed.parse::<f32>()?);
    }
    if values.len() % 2 != 0 {
        bail!("invalid points list");
    }
    Ok(values
        .chunks_exact(2)
        .map(|chunk| (chunk[0], chunk[1]))
        .collect())
}

fn tokenize_path(data: &str) -> Result<Vec<PathToken>> {
    let mut tokens = Vec::new();
    let mut number = String::new();
    for ch in data.chars() {
        if ch.is_ascii_alphabetic() {
            if !number.is_empty() {
                tokens.push(PathToken::Number(number.parse()?));
                number.clear();
            }
            tokens.push(PathToken::Command(ch));
        } else if ch.is_ascii_digit() || matches!(ch, '-' | '+' | '.') {
            if (ch == '-' || ch == '+') && !number.is_empty() {
                tokens.push(PathToken::Number(number.parse()?));
                number.clear();
            }
            number.push(ch);
        } else if !number.is_empty() {
            tokens.push(PathToken::Number(number.parse()?));
            number.clear();
        }
    }
    if !number.is_empty() {
        tokens.push(PathToken::Number(number.parse()?));
    }
    Ok(tokens)
}

#[derive(Clone, Copy)]
enum PathToken {
    Command(char),
    Number(f32),
}

fn next_number(tokens: &[PathToken], index: &mut usize) -> Result<f32> {
    match tokens.get(*index).copied() {
        Some(PathToken::Number(value)) => {
            *index += 1;
            Ok(value)
        }
        _ => bail!("expected path number"),
    }
}

fn parse_path_segments(data: &str) -> Result<Vec<Segment>> {
    let tokens = tokenize_path(data)?;
    let mut segments = Vec::new();
    let mut index = 0usize;
    let mut current = (0.0f32, 0.0f32);
    let mut subpath_start = current;
    let mut command = ' ';

    while index < tokens.len() {
        if let Some(PathToken::Command(next)) = tokens.get(index).copied() {
            command = next;
            index += 1;
        } else if command == ' ' {
            bail!("path data missing command");
        }

        match command {
            'M' | 'm' => {
                let mut first = true;
                while matches!(tokens.get(index), Some(PathToken::Number(_))) {
                    let x = next_number(&tokens, &mut index)?;
                    let y = next_number(&tokens, &mut index)?;
                    let next = if command == 'm' {
                        (current.0 + x, current.1 + y)
                    } else {
                        (x, y)
                    };
                    if first {
                        current = next;
                        subpath_start = next;
                        first = false;
                    } else {
                        segments.push(Segment {
                            start: current,
                            end: next,
                        });
                        current = next;
                    }
                }
            }
            'L' | 'l' => {
                while matches!(tokens.get(index), Some(PathToken::Number(_))) {
                    let x = next_number(&tokens, &mut index)?;
                    let y = next_number(&tokens, &mut index)?;
                    let next = if command == 'l' {
                        (current.0 + x, current.1 + y)
                    } else {
                        (x, y)
                    };
                    segments.push(Segment {
                        start: current,
                        end: next,
                    });
                    current = next;
                }
            }
            'H' | 'h' => {
                while matches!(tokens.get(index), Some(PathToken::Number(_))) {
                    let value = next_number(&tokens, &mut index)?;
                    let next = if command == 'h' {
                        (current.0 + value, current.1)
                    } else {
                        (value, current.1)
                    };
                    segments.push(Segment {
                        start: current,
                        end: next,
                    });
                    current = next;
                }
            }
            'V' | 'v' => {
                while matches!(tokens.get(index), Some(PathToken::Number(_))) {
                    let value = next_number(&tokens, &mut index)?;
                    let next = if command == 'v' {
                        (current.0, current.1 + value)
                    } else {
                        (current.0, value)
                    };
                    segments.push(Segment {
                        start: current,
                        end: next,
                    });
                    current = next;
                }
            }
            'Z' | 'z' => {
                segments.push(Segment {
                    start: current,
                    end: subpath_start,
                });
                current = subpath_start;
            }
            other => bail!("unsupported path command: {other}"),
        }
    }

    Ok(segments)
}

fn circle_segments(cx: f32, cy: f32, radius: f32, steps: usize) -> Vec<Segment> {
    let mut segments = Vec::with_capacity(steps);
    let tau = std::f32::consts::PI * 2.0;
    let step = tau / steps as f32;
    for index in 0..steps {
        let a = index as f32 * step;
        let b = (index as f32 + 1.0) * step;
        segments.push(Segment {
            start: (cx + a.cos() * radius, cy + a.sin() * radius),
            end: (cx + b.cos() * radius, cy + b.sin() * radius),
        });
    }
    segments
}

fn rect_segments(x: f32, y: f32, width: f32, height: f32) -> Vec<Segment> {
    let left = x;
    let top = y;
    let right = x + width;
    let bottom = y + height;
    vec![
        Segment {
            start: (left, top),
            end: (right, top),
        },
        Segment {
            start: (right, top),
            end: (right, bottom),
        },
        Segment {
            start: (right, bottom),
            end: (left, bottom),
        },
        Segment {
            start: (left, bottom),
            end: (left, top),
        },
    ]
}

fn icon_segments(svg_path: &Path) -> Result<(f32, f32, Vec<Segment>)> {
    let source = fs::read_to_string(svg_path)?;
    let doc = roxmltree::Document::parse(&source)?;
    let root = doc.root_element();
    let viewport = root
        .attribute("viewBox")
        .and_then(|view_box| {
            let values = view_box
                .split_ascii_whitespace()
                .filter_map(|part| part.parse::<f32>().ok())
                .collect::<Vec<_>>();
            (values.len() == 4).then_some(values)
        })
        .map(|values| values[2])
        .unwrap_or(24.0);
    let stroke_width = root
        .attribute("stroke-width")
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(2.0);

    let mut segments = Vec::new();
    for node in root.children().filter(|node| node.is_element()) {
        match node.tag_name().name() {
            "path" => {
                let data = node.attribute("d").context("path missing d")?;
                segments.extend(parse_path_segments(data)?);
            }
            "line" => segments.push(Segment {
                start: (attr_f32(node, "x1", 0.0)?, attr_f32(node, "y1", 0.0)?),
                end: (attr_f32(node, "x2", 0.0)?, attr_f32(node, "y2", 0.0)?),
            }),
            "polyline" | "polygon" => {
                let points =
                    parse_points(node.attribute("points").context("shape missing points")?)?;
                for pair in points.windows(2) {
                    segments.push(Segment {
                        start: pair[0],
                        end: pair[1],
                    });
                }
                if node.tag_name().name() == "polygon" && points.len() > 2 {
                    segments.push(Segment {
                        start: *points.last().unwrap(),
                        end: points[0],
                    });
                }
            }
            "circle" => segments.extend(circle_segments(
                attr_f32(node, "cx", 0.0)?,
                attr_f32(node, "cy", 0.0)?,
                attr_f32(node, "r", 0.0)?,
                16,
            )),
            "rect" => segments.extend(rect_segments(
                attr_f32(node, "x", 0.0)?,
                attr_f32(node, "y", 0.0)?,
                attr_f32(node, "width", 0.0)?,
                attr_f32(node, "height", 0.0)?,
            )),
            other => bail!("unsupported svg node: {other}"),
        }
    }

    Ok((viewport, stroke_width, segments))
}

fn main() -> Result<()> {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let icons_dir = manifest_dir.join("icons");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);
    let out_file = out_dir.join("icons_gen.rs");

    println!("cargo:rerun-if-changed={}", icons_dir.display());

    let mut output = String::new();
    output.push_str("pub mod icons {\n");
    output.push_str("    use arcui_core::{LineSegment, Vec2, VectorIcon};\n\n");

    for path in read_icon_paths(&icons_dir)? {
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .context("invalid icon name")?;
        let function_name = icon_id_for(stem);
        let constant_name = function_name.to_ascii_uppercase();
        let (viewport, stroke_width, segments) =
            icon_segments(&path).with_context(|| format!("parsing {}", path.display()))?;

        output.push_str(&format!(
            "    static {constant_name}_SEGMENTS: &[LineSegment] = &[\n"
        ));
        for segment in segments {
            output.push_str(&format!(
                "        LineSegment {{ start: Vec2::new({:.4}, {:.4}), end: Vec2::new({:.4}, {:.4}) }},\n",
                segment.start.0, segment.start.1, segment.end.0, segment.end.1
            ));
        }
        output.push_str("    ];\n");
        output.push_str(&format!(
            "    static {constant_name}: VectorIcon = VectorIcon {{ viewport: {:.4}, stroke_width: {:.4}, segments: {constant_name}_SEGMENTS }};\n",
            viewport, stroke_width
        ));
        output.push_str("    #[must_use]\n");
        output.push_str(&format!(
            "    pub fn {function_name}() -> &'static VectorIcon {{ &{constant_name} }}\n\n"
        ));
    }

    output.push_str("}\n");

    let mut file = fs::File::create(out_file)?;
    file.write_all(output.as_bytes())?;
    Ok(())
}
