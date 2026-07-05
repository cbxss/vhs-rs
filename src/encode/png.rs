//! PNG output for rendered frames.

use std::io::{self, Write};
use std::path::Path;

use crate::render::Canvas;

fn encode<W: Write>(w: W, canvas: &Canvas) -> io::Result<()> {
    let mut encoder = png::Encoder::new(w, canvas.w as u32, canvas.h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(io::Error::other)?;
    writer
        .write_image_data(&canvas.buf)
        .map_err(io::Error::other)?;
    writer.finish().map_err(io::Error::other)
}

/// Writes the canvas to `path` as a non-interlaced RGBA8 PNG.
pub fn write_png(path: &Path, canvas: &Canvas) -> io::Result<()> {
    let file = std::fs::File::create(path)?;
    encode(io::BufWriter::new(file), canvas)
}

/// Encodes the canvas to PNG bytes in memory.
pub fn png_bytes(canvas: &Canvas) -> Vec<u8> {
    let mut out = Vec::new();
    encode(&mut out, canvas).expect("in-memory PNG encoding cannot fail");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Rgb;

    #[test]
    fn png_roundtrip_preserves_dims_and_pixels() {
        let mut canvas = Canvas::new(37, 21);
        canvas.fill(Rgb(1, 2, 3));
        canvas.set_px(5, 7, Rgb(200, 100, 50));

        let bytes = png_bytes(&canvas);
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();

        assert_eq!((info.width, info.height), (37, 21));
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(&buf[..info.buffer_size()], canvas.buf.as_slice());
    }

    /// Visual smoke check: renders a full VHS-styled frame to a temp path.
    /// Run with `cargo test --lib smoke_render -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn smoke_render_full_frame() {
        use crate::render::{BarStyle, RenderOptions, Renderer};
        use crate::snapshot::{Cell, CellAttrs, Color, Cursor, GridSnapshot};

        let opts = RenderOptions {
            window_bar: Some(BarStyle::Colorful),
            border_radius: 8,
            margin: 20,
            margin_fill: MarginFill::Color(Rgb(0x5b, 0x56, 0xe0)),
            ..RenderOptions::default()
        };
        use crate::render::MarginFill;
        let theme = crate::theme::load_builtin("Dracula").unwrap();
        let mut r = Renderer::new(opts, theme);
        let (cols, rows) = r.term_size();
        let mut cells = vec![Cell::default(); cols * rows];
        let lines: [(&str, Option<Color>, CellAttrs); 5] = [
            ("> ls --color", None, CellAttrs::default()),
            (
                "src assets Cargo.toml",
                Some(Color::Indexed(4)),
                CellAttrs {
                    bold: true,
                    ..Default::default()
                },
            ),
            (
                "underline + italic sample",
                Some(Color::Indexed(2)),
                CellAttrs {
                    italic: true,
                    underline: true,
                    ..Default::default()
                },
            ),
            (
                "faint strikethrough",
                Some(Color::Indexed(3)),
                CellAttrs {
                    faint: true,
                    strikethrough: true,
                    ..Default::default()
                },
            ),
            ("> ", None, CellAttrs::default()),
        ];
        for (row, (text, fg, attrs)) in lines.iter().enumerate() {
            for (col, ch) in text.chars().enumerate() {
                cells[row * cols + col] = Cell {
                    ch,
                    fg: *fg,
                    bg: None,
                    attrs: *attrs,
                    width: 1,
                };
            }
        }
        let snap = GridSnapshot {
            cols,
            rows,
            cells,
            cursor: Cursor {
                col: 2,
                row: 4,
                visible: true,
            },
        };
        let canvas = r.render(&snap);
        let path = std::env::temp_dir().join("vhs_rs-smoke.png");
        write_png(&path, canvas).unwrap();
        println!("wrote {}", path.display());
    }

    #[test]
    fn write_png_creates_decodable_file() {
        let mut canvas = Canvas::new(8, 8);
        canvas.fill(Rgb(9, 9, 9));
        let dir = std::env::temp_dir();
        let path = dir.join(format!("vhs_rs-png-test-{}.png", std::process::id()));
        write_png(&path, &canvas).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let mut reader = png::Decoder::new(std::io::BufReader::new(file))
            .read_info()
            .unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (8, 8));
        std::fs::remove_file(&path).ok();
    }
}
