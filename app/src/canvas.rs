//! What the window is drawn on: a grid of pixels, and the few shapes and the text the screens are made of. Drawn by
//! the program itself, so it looks the same on every system and needs nothing from any of them.

use fontdue::Font;

/// A colour as 0xRRGGBB.
pub type Colour = u32;

pub struct Canvas {
    pub width: usize,
    pub height: usize,
    /// One pixel each, as 0x00RRGGBB, rows from the top.
    pub pixels: Vec<u32>,
}

/// A picture as red, green, blue and opacity, four bytes a pixel.
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl Picture {
    pub fn from_png(bytes: &[u8]) -> Option<Picture> {
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().ok()?;
        let mut rgba = vec![0; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut rgba).ok()?;
        (info.color_type == png::ColorType::Rgba && info.bit_depth == png::BitDepth::Eight).then_some(Picture { width: info.width as usize, height: info.height as usize, rgba })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// Every text is drawn this much larger than the size the screens ask for: the sizes there are those of the layout
/// as first drawn, which read small on a real display. Boxes, margins and spacing are not scaled with it.
pub const TEXT_SCALE: f32 = 1.16;

/// How much of a pixel a letter covers, made a little fuller. Light letters on a dark ground look thinner than
/// they are when their edges are mixed in evenly; lifting the part-covered pixels gives the strokes their weight
/// back. Fully covered and untouched pixels stay as they are.
fn fuller(coverage: u8) -> u32 {
    static CURVE: std::sync::OnceLock<[u8; 256]> = std::sync::OnceLock::new();
    let curve = CURVE.get_or_init(|| std::array::from_fn(|covered| ((covered as f32 / 255.0).powf(0.72) * 255.0).round() as u8));
    u32::from(curve[usize::from(coverage)])
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Canvas {
        Canvas { width, height, pixels: vec![0; width * height] }
    }

    pub fn clear(&mut self, colour: Colour) {
        self.pixels.fill(colour);
    }

    /// Mixes a colour into one pixel: `alpha` 255 replaces it, 0 leaves it.
    fn blend(&mut self, x: i32, y: i32, colour: Colour, alpha: u32) {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height || alpha == 0 {
            return;
        }
        let at = y as usize * self.width + x as usize;
        let under = self.pixels[at];
        let mix = |shift: u32| (((colour >> shift) & 0xff) * alpha + ((under >> shift) & 0xff) * (255 - alpha)) / 255;
        self.pixels[at] = (mix(16) << 16) | (mix(8) << 8) | mix(0);
    }

    /// A rectangle with rounded corners; radius 0 is a plain one. The curves are smoothed by sampling each pixel on
    /// them sixteen times; everything else is filled outright.
    pub fn rounded(&mut self, rect: Rect, radius: i32, colour: Colour) {
        let radius = radius.min(rect.w / 2).min(rect.h / 2).max(0);
        let r = radius as f32;
        for y in rect.y.max(0)..(rect.y + rect.h).min(self.height as i32) {
            for x in rect.x.max(0)..(rect.x + rect.w).min(self.width as i32) {
                let (lx, ly) = (x - rect.x, y - rect.y);
                let in_corner = (lx < radius || lx >= rect.w - radius) && (ly < radius || ly >= rect.h - radius);
                if !in_corner {
                    self.pixels[y as usize * self.width + x as usize] = colour;
                    continue;
                }
                let cx = if lx < radius { r } else { rect.w as f32 - r };
                let cy = if ly < radius { r } else { rect.h as f32 - r };
                let mut inside = 0;
                for sample in 0..16 {
                    let (px, py) = (lx as f32 + (sample % 4) as f32 / 4.0 + 0.125, ly as f32 + (sample / 4) as f32 / 4.0 + 0.125);
                    if (px - cx).powi(2) + (py - cy).powi(2) <= r * r {
                        inside += 1;
                    }
                }
                self.blend(x, y, colour, inside * 255 / 16);
            }
        }
    }

    /// A rounded rectangle drawn as an outline of the given thickness around a fill.
    pub fn bordered(&mut self, rect: Rect, radius: i32, thickness: i32, border: Colour, fill: Colour) {
        self.rounded(rect, radius, border);
        self.rounded(Rect { x: rect.x + thickness, y: rect.y + thickness, w: rect.w - 2 * thickness, h: rect.h - 2 * thickness }, radius - thickness, fill);
    }

    /// A straight stroke with round ends.
    pub fn line(&mut self, from: (f32, f32), to: (f32, f32), thickness: f32, colour: Colour) {
        let half = thickness / 2.0;
        let (left, right) = ((from.0.min(to.0) - half).floor() as i32, (from.0.max(to.0) + half).ceil() as i32);
        let (top, bottom) = ((from.1.min(to.1) - half).floor() as i32, (from.1.max(to.1) + half).ceil() as i32);
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = (dx * dx + dy * dy).max(0.0001);
        for y in top..=bottom {
            for x in left..=right {
                let mut inside = 0;
                for sample in 0..16 {
                    let (px, py) = (x as f32 + (sample % 4) as f32 / 4.0 + 0.125, y as f32 + (sample / 4) as f32 / 4.0 + 0.125);
                    // How far along the stroke the nearest point is, held to its two ends.
                    let along = (((px - from.0) * dx + (py - from.1) * dy) / length).clamp(0.0, 1.0);
                    let (nx, ny) = (from.0 + along * dx, from.1 + along * dy);
                    if (px - nx).powi(2) + (py - ny).powi(2) <= half * half {
                        inside += 1;
                    }
                }
                self.blend(x, y, colour, inside * 255 / 16);
            }
        }
    }

    /// A picture with its own transparency, drawn `size` pixels square, each pixel the average of the ones it covers.
    pub fn picture(&mut self, picture: &Picture, x: i32, y: i32, size: i32) {
        for row in 0..size {
            for column in 0..size {
                let span = |at: i32, whole: usize| ((at as usize * whole / size as usize), (((at + 1) as usize * whole).div_ceil(size as usize)).min(whole));
                let ((left, right), (top, bottom)) = (span(column, picture.width), span(row, picture.height));
                let (mut colours, mut opacity) = ([0u32; 3], 0u32);
                for sy in top..bottom.max(top + 1) {
                    for sx in left..right.max(left + 1) {
                        let at = (sy.min(picture.height - 1) * picture.width + sx.min(picture.width - 1)) * 4;
                        let alpha = u32::from(picture.rgba[at + 3]);
                        for (sum, value) in colours.iter_mut().zip(&picture.rgba[at..at + 3]) {
                            *sum += u32::from(*value) * alpha;
                        }
                        opacity += alpha;
                    }
                }
                let count = ((bottom.max(top + 1) - top) * (right.max(left + 1) - left)) as u32;
                // Nothing to draw where every pixel covered is fully see-through.
                if let [Some(red), Some(green), Some(blue)] = colours.map(|sum| sum.checked_div(opacity)) {
                    self.blend(x + column, y + row, (red << 16) | (green << 8) | blue, opacity / count);
                }
            }
        }
    }

    pub fn circle(&mut self, cx: i32, cy: i32, radius: i32, colour: Colour) {
        self.rounded(Rect { x: cx - radius, y: cy - radius, w: radius * 2, h: radius * 2 }, radius, colour);
    }

    /// Draws text with its baseline at `y`, starting at `x`. Returns where it ended.
    pub fn text(&mut self, font: &Font, x: i32, y: i32, size: f32, colour: Colour, text: &str) -> i32 {
        let size = size * TEXT_SCALE;
        let mut pen = x as f32;
        for character in text.chars() {
            let (metrics, coverage) = font.rasterize(character, size);
            let top = y - metrics.ymin - metrics.height as i32;
            for row in 0..metrics.height {
                for column in 0..metrics.width {
                    self.blend(pen as i32 + metrics.xmin + column as i32, top + row as i32, colour, fuller(coverage[row * metrics.width + column]));
                }
            }
            pen += metrics.advance_width;
        }
        pen as i32
    }

    pub fn text_width(font: &Font, size: f32, text: &str) -> i32 {
        let size = size * TEXT_SCALE;
        text.chars().map(|character| font.metrics(character, size).advance_width).sum::<f32>() as i32
    }

    /// Text centred on `centre_x`.
    pub fn centred(&mut self, font: &Font, centre_x: i32, y: i32, size: f32, colour: Colour, text: &str) {
        self.text(font, centre_x - Canvas::text_width(font, size, text) / 2, y, size, colour, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rounded_rectangle_fills_its_middle_and_leaves_its_corners() {
        let mut canvas = Canvas::new(40, 40);
        canvas.rounded(Rect { x: 0, y: 0, w: 40, h: 40 }, 12, 0xff0000);
        assert_eq!(canvas.pixels[20 * 40 + 20], 0xff0000);
        assert_eq!(canvas.pixels[0], 0, "the very corner is outside the curve");
        assert_eq!(canvas.pixels[20], 0xff0000, "the middle of the top edge is inside");
        let edge = canvas.pixels[3 * 40 + 2];
        assert!(edge == 0 || (edge >> 16) > 0, "edge pixels are the colour or a share of it");
        canvas.rounded(Rect { x: 30, y: 30, w: 50, h: 50 }, 0, 0x00ff00);
        assert_eq!(canvas.pixels[39 * 40 + 39], 0x00ff00, "drawing past the edge is cut, not a crash");
        canvas.bordered(Rect { x: 0, y: 0, w: 40, h: 40 }, 8, 2, 0x0000ff, 0x111111);
        assert_eq!((canvas.pixels[20], canvas.pixels[20 * 40 + 20]), (0x0000ff, 0x111111), "an outline around a fill");
        canvas.line((5.0, 20.0), (35.0, 20.0), 4.0, 0xffffff);
        assert_eq!((canvas.pixels[20 * 40 + 20], canvas.pixels[10 * 40 + 20]), (0xffffff, 0x111111), "a stroke where it was drawn and nowhere else");
    }

    #[test]
    fn the_icon_is_a_picture_with_see_through_corners() {
        let icon = Picture::from_png(include_bytes!("../assets/icon.png")).expect("the built-in icon reads");
        assert_eq!((icon.width, icon.height), (256, 256));
        let mut canvas = Canvas::new(64, 64);
        canvas.clear(0x123456);
        canvas.picture(&icon, 0, 0, 64);
        assert_eq!(canvas.pixels[0], 0x123456, "the corner shows what is behind");
        assert_ne!(canvas.pixels[32 * 64 + 32], 0x123456, "the middle is the icon");
    }

    #[test]
    fn text_leaves_ink_and_reports_its_width() {
        let font = Font::from_bytes(include_bytes!("../assets/NotoSans-Regular.ttf") as &[u8], fontdue::FontSettings::default()).unwrap();
        let mut canvas = Canvas::new(200, 40);
        let end = canvas.text(&font, 10, 28, 20.0, 0xffffff, "Pair");
        assert!(canvas.pixels.contains(&0xffffff) && canvas.pixels.iter().any(|pixel| *pixel != 0 && *pixel != 0xffffff), "solid ink and smoothed edges");
        assert!(end > 40 && end < 80, "{end}");
        assert_eq!(end - 10, Canvas::text_width(&font, 20.0, "Pair"));
        assert!(Canvas::text_width(&font, 20.0, "Pair with master") > Canvas::text_width(&font, 20.0, "Pair"));
    }
}
