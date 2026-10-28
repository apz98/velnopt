//! Opaque software rendering: bounded geometry and shaped Unicode text.

use std::borrow::Cow;

use cosmic_text::{
    Attrs, Buffer, Color, Ellipsize, EllipsizeHeightLimit, Family, FontSystem, Metrics, Shaping,
    SwashCache, Wrap,
};
use velnopt_config::{Color as TextColor, Config, Position};

/// Resolved text for the renderer. It knows nothing about widget kinds or the time.
#[derive(Clone, Debug)]
pub struct Label<'a> {
    /// Text to shape and draw. Borrowed when no formatting was needed.
    pub text: Cow<'a, str>,
    /// Horizontal information group.
    pub position: Position,
    /// Foreground override, or the configured default when absent.
    pub color: Option<TextColor>,
    /// Continue the previous label's item, with a tight gap instead of the widget gap.
    pub joined: bool,
    /// Draw a keyboard focus ring around this label.
    pub focused: bool,
    /// Optional image drawn at line height before the text.
    pub icon: Option<Image<'a>>,
}

/// Straight-alpha RGBA8 pixels, validated by [`Image::new`].
#[derive(Clone, Copy, Debug)]
pub struct Image<'a> {
    width: u16,
    height: u16,
    rgba: &'a [u8],
}

impl<'a> Image<'a> {
    /// Wrap RGBA8 pixels. Returns `None` if the size is empty or doesn't match the data.
    pub fn new(width: u16, height: u16, rgba: &'a [u8]) -> Option<Self> {
        (width > 0 && height > 0 && rgba.len() == usize::from(width) * usize::from(height) * 4)
            .then_some(Self {
                width,
                height,
                rgba,
            })
    }
}

/// Horizontal physical extent of a drawn label, used for pointer hit testing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Span {
    /// First physical column, inclusive.
    pub start: i32,
    /// Last physical column, exclusive.
    pub end: i32,
}

/// Focus ring thickness and inset from the label, in logical pixels.
const FOCUS_RING: i32 = 2;
const FOCUS_INSET: i32 = 4;

/// Cap on frame size, checked before allocating shared memory or looping over pixels.
const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

/// Validated physical size of an opaque, native-endian XRGB8888 frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Geometry {
    width: i32,
    height: i32,
    stride: i32,
    bytes: usize,
    scale: i32,
}

impl Geometry {
    /// Check the logical size and convert it to physical pixels.
    ///
    /// # Errors
    ///
    /// Fails on zero dimensions, scales outside 1 to 8, arithmetic overflow, and
    /// frames larger than 32 MiB.
    pub fn new(width: u32, height: u32, scale: i32) -> Result<Self, &'static str> {
        if width == 0 || height == 0 || !(1..=8).contains(&scale) {
            return Err("surface dimensions must be positive and scale must be between 1 and 8");
        }
        let physical_width = u64::from(width) * scale as u64;
        let physical_height = u64::from(height) * scale as u64;
        let stride = physical_width
            .checked_mul(4)
            .ok_or("surface stride overflow")?;
        let bytes = stride
            .checked_mul(physical_height)
            .ok_or("surface size overflow")?;
        if bytes > MAX_FRAME_BYTES as u64
            || stride > i32::MAX as u64
            || physical_height > i32::MAX as u64
        {
            return Err(
                "surface exceeds the 32 MiB frame limit; reduce panel height or output scale",
            );
        }
        Ok(Self {
            width: physical_width as i32,
            height: physical_height as i32,
            stride: stride as i32,
            bytes: bytes as usize,
            scale,
        })
    }

    /// Physical width in pixels.
    pub fn width(self) -> i32 {
        self.width
    }

    /// Physical height in pixels.
    pub fn height(self) -> i32 {
        self.height
    }

    /// Number of bytes per row.
    pub fn stride(self) -> i32 {
        self.stride
    }

    /// Exact number of bytes the frame needs.
    pub fn byte_len(self) -> usize {
        self.bytes
    }
}

/// Software panel renderer. Font and glyph caches are shared.
pub struct Renderer {
    fonts: FontSystem,
    cache: SwashCache,
}

impl Renderer {
    /// Drop cached glyphs after a new configuration is accepted.
    pub fn clear_cache(&mut self) {
        self.cache = SwashCache::new();
    }

    /// Find the system fonts and set up the renderer.
    ///
    /// # Errors
    ///
    /// Returns an error when no system fonts are available.
    pub fn new() -> Result<Self, &'static str> {
        let fonts = FontSystem::new();
        if fonts.db().is_empty() {
            return Err("no system fonts found; install a font such as DejaVu Sans or Noto Sans");
        }
        Ok(Self {
            fonts,
            cache: SwashCache::new(),
        })
    }

    /// Draw the resolved labels with the validated styling, as an opaque XRGB8888 panel.
    ///
    /// Returns each label's visible physical span, in label order. A label clipped
    /// out of its group entirely has none.
    ///
    /// # Errors
    ///
    /// Rejects a buffer whose length differs from [`Geometry::byte_len`], before
    /// touching it. Geometry can only be built through validation.
    pub fn draw(
        &mut self,
        pixels: &mut [u8],
        geometry: Geometry,
        config: &Config,
        labels: &[Label<'_>],
    ) -> Result<Vec<Option<Span>>, &'static str> {
        if pixels.len() != geometry.bytes {
            return Err("pixel buffer length does not match frame geometry");
        }
        let panel = config.panel();
        let appearance = panel.appearance();
        let [r, g, b] = appearance.background().rgb();
        let background =
            (0xff000000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)).to_ne_bytes();
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&background);
        }

        let scale = geometry.scale as f32;
        let padding = (i32::from(appearance.padding()) * geometry.scale).min(geometry.width / 2);
        let available = geometry.width - 2 * padding;
        let font_size = f32::from(appearance.font_size()) * scale;
        let line_height = font_size * 1.25;
        let text_y = ((geometry.height as f32 - line_height) / 2.0).round() as i32;
        let mut spans = vec![None; labels.len()];
        let icon_size = (line_height.round() as i32).min(geometry.height);
        let family = match appearance.font() {
            "sans-serif" => Family::SansSerif,
            "serif" => Family::Serif,
            "monospace" => Family::Monospace,
            name => Family::Name(name),
        };

        let gap = i32::from(appearance.gap()) * geometry.scale;
        // about two spaces wide, like the old single-label workspace text
        let joined_gap = (font_size * 0.6).round() as i32;
        let gap_before = |label: &Label<'_>, first: bool| match (first, label.joined) {
            (true, _) => 0,
            (false, true) => joined_gap,
            (false, false) => gap,
        };
        let shape = |fonts: &mut FontSystem, buffer: &mut Buffer, width: i32| {
            buffer.set_size(Some(width as f32), Some(geometry.height as f32));
            buffer.shape_until_scroll(fonts, false);
            buffer
                .layout_runs()
                .map(|run| run.line_w)
                .fold(0.0_f32, f32::max)
                .ceil() as i32
        };
        // shaped buffer, measured width, icon width, color, label index, label
        type LaidOut<'a, 'b> = (Buffer, i32, i32, [u8; 3], usize, &'b Label<'a>);
        let total = |laid_out: &[LaidOut<'_, '_>]| -> i32 {
            laid_out
                .iter()
                .enumerate()
                .map(|(index, (_, width, _, _, _, label))| width + gap_before(label, index == 0))
                .sum()
        };

        let mut groups = [Vec::new(), Vec::new(), Vec::new()];
        for (laid_out, position) in
            groups
                .iter_mut()
                .zip([Position::Left, Position::Center, Position::Right])
        {
            for (label_index, label) in labels
                .iter()
                .enumerate()
                .filter(|(_, label)| label.position == position)
            {
                let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(font_size, line_height));
                buffer.set_wrap(Wrap::None);
                buffer.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
                buffer.set_text(
                    &label.text,
                    &Attrs::new().family(family),
                    Shaping::Advanced,
                    None,
                );
                let text_width = shape(&mut self.fonts, &mut buffer, available.max(0));
                // icons are square at line height, with a gap before any text
                let icon_width = match (label.icon, label.text.is_empty()) {
                    (None, _) => 0,
                    (Some(_), true) => icon_size,
                    (Some(_), false) => icon_size + (font_size * 0.3).round() as i32,
                };
                laid_out.push((
                    buffer,
                    icon_width + text_width,
                    icon_width,
                    label.color.unwrap_or(appearance.foreground()).rgb(),
                    label_index,
                    label,
                ));
            }
        }
        let natural = [total(&groups[0]), total(&groups[1]), total(&groups[2])];
        let bounds = regions(padding, padding + available, gap, natural);

        for ((mut laid_out, position), (start, end)) in groups
            .into_iter()
            .zip([Position::Left, Position::Center, Position::Right])
            .zip(bounds)
        {
            let width = end - start;
            if width <= 0 {
                continue;
            }
            // only a group that overflows its region pays for ellipsizing
            if total(&laid_out) > width {
                for (buffer, measured, icon_width, ..) in &mut laid_out {
                    *measured = (*icon_width + shape(&mut self.fonts, buffer, width)).min(width);
                }
            }
            let total = total(&laid_out);
            let mut x = match position {
                Position::Left => start,
                Position::Center => start + (width - total) / 2,
                Position::Right => end - total,
            };
            for (index, (mut buffer, measured, icon_width, [r, g, b], label_index, label)) in
                laid_out.into_iter().enumerate()
            {
                x += gap_before(label, index == 0);
                if let Some(icon) = label.icon {
                    let top = ((geometry.height - icon_size) / 2).max(0);
                    draw_icon(pixels, geometry, icon, x, top, icon_size, (start, end));
                }
                let text_x = x + icon_width;
                buffer.draw(
                    &mut self.fonts,
                    &mut self.cache,
                    Color::rgb(r, g, b),
                    |px, py, w, h, color| {
                        for row in py..py.saturating_add(h as i32) {
                            for col in px..px.saturating_add(w as i32) {
                                let target_x = col.saturating_add(text_x);
                                let target_y = row.saturating_add(text_y);
                                if target_x < start
                                    || target_x >= end
                                    || target_y < 0
                                    || target_y >= geometry.height
                                {
                                    continue;
                                }
                                let offset = target_y as usize * geometry.stride as usize
                                    + target_x as usize * 4;
                                blend(&mut pixels[offset..offset + 4], color);
                            }
                        }
                    },
                );
                let visible = (x.max(start), (x + measured).min(end));
                if visible.0 < visible.1 {
                    spans[label_index] = Some(Span {
                        start: visible.0,
                        end: visible.1,
                    });
                    if label.focused {
                        let inset = FOCUS_INSET * geometry.scale;
                        let ring = Rectangle {
                            left: (x - inset).max(start),
                            right: (x + measured + inset).min(end),
                            top: (text_y - inset).max(0),
                            bottom: (text_y + line_height.ceil() as i32 + inset)
                                .min(geometry.height),
                        };
                        ring.outline(pixels, geometry, FOCUS_RING * geometry.scale, [r, g, b]);
                    }
                }
                x += measured;
            }
        }
        Ok(spans)
    }
}

/// Split the row `start..end` between the left, center, and right groups by natural width.
///
/// Sides keep their natural width, but when both overflow each keeps at least half the row.
/// The center fills the space between them, and sits on the row's middle when it fits.
fn regions(start: i32, end: i32, gap: i32, [left, center, right]: [i32; 3]) -> [(i32, i32); 3] {
    let available = (end - start).max(0);
    let left = left.min((available / 2).max(available - right));
    let right = right.min(available - left);
    let low = start + left + if left > 0 { gap } else { 0 };
    let high = end - right - if right > 0 { gap } else { 0 };
    let center = center.min(high - low).max(0);
    let center_start = ((start + end - center) / 2).clamp(low, (high - center).max(low));
    [
        (start, start + left),
        (center_start, center_start + center),
        (end - right, end),
    ]
}

/// A clipped physical rectangle; `right` and `bottom` are exclusive.
struct Rectangle {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

impl Rectangle {
    /// Draw an opaque outline inside the rectangle, clipped to the frame.
    fn outline(&self, pixels: &mut [u8], geometry: Geometry, thickness: i32, [r, g, b]: [u8; 3]) {
        let color =
            (0xff000000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)).to_ne_bytes();
        let left = self.left.max(0);
        let right = self.right.min(geometry.width);
        let top = self.top.max(0);
        let bottom = self.bottom.min(geometry.height);
        for y in top..bottom {
            for x in left..right {
                let edge = x < left + thickness
                    || x >= right - thickness
                    || y < top + thickness
                    || y >= bottom - thickness;
                if edge {
                    let offset = y as usize * geometry.stride as usize + x as usize * 4;
                    pixels[offset..offset + 4].copy_from_slice(&color);
                }
            }
        }
    }
}

/// Scale an image into a `size` square at (`left`, `top`) with nearest-neighbour
/// sampling, blending over the frame and clipping to the group's columns.
fn draw_icon(
    pixels: &mut [u8],
    geometry: Geometry,
    icon: Image<'_>,
    left: i32,
    top: i32,
    size: i32,
    (start, end): (i32, i32),
) {
    for row in 0..size {
        let y = top + row;
        if !(0..geometry.height).contains(&y) {
            continue;
        }
        let source_y = (row as usize * usize::from(icon.height)) / size as usize;
        for col in 0..size {
            let x = left + col;
            if x < start || x >= end || !(0..geometry.width).contains(&x) {
                continue;
            }
            let source_x = (col as usize * usize::from(icon.width)) / size as usize;
            let source = (source_y * usize::from(icon.width) + source_x) * 4;
            let [r, g, b, a] = [
                icon.rgba[source],
                icon.rgba[source + 1],
                icon.rgba[source + 2],
                icon.rgba[source + 3],
            ];
            let offset = y as usize * geometry.stride as usize + x as usize * 4;
            blend(&mut pixels[offset..offset + 4], Color::rgba(r, g, b, a));
        }
    }
}

fn blend(pixel: &mut [u8], color: Color) {
    let [r, g, b, a] = color.as_rgba();
    let destination = u32::from_ne_bytes([pixel[0], pixel[1], pixel[2], pixel[3]]);
    let alpha = u32::from(a);
    let channel = |source: u8, shift: u32| -> u32 {
        (u32::from(source) * alpha + ((destination >> shift) & 255) * (255 - alpha) + 127) / 255
    };
    let result: u32 = 0xff000000 | (channel(r, 16) << 16) | (channel(g, 8) << 8) | channel(b, 0);
    pixel.copy_from_slice(&result.to_ne_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_rejects_invalid_dimensions_and_overflow_before_allocation() {
        for (width, height, scale) in [
            (0, 32, 1),
            (1920, 0, 1),
            (1920, 32, 0),
            (1920, 32, -1),
            (1920, 32, 9),
            (u32::MAX, u32::MAX, 8),
            (16384, 1024, 4),
        ] {
            assert!(Geometry::new(width, height, scale).is_err());
        }
        let geometry = Geometry::new(1920, 32, 2).expect("normal HiDPI panel fits");
        assert_eq!(geometry.bytes, 3840 * 64 * 4);
    }

    #[test]
    fn groups_take_natural_widths_and_the_center_yields() {
        // everything fits: sides at the edges, center on the row's middle
        assert_eq!(
            regions(0, 1000, 10, [100, 200, 300]),
            [(0, 100), (400, 600), (700, 1000)]
        );
        // a wide right group pushes the center off the middle, then shrinks it
        assert_eq!(
            regions(0, 1000, 10, [100, 400, 500]),
            [(0, 100), (110, 490), (500, 1000)]
        );
        // both sides overflow: each keeps half, the center gets nothing
        assert_eq!(
            regions(0, 1000, 10, [900, 50, 900]),
            [(0, 500), (510, 510), (500, 1000)]
        );
        // a narrow side hands its spare room to the other
        assert_eq!(
            regions(0, 1000, 10, [100, 0, 2000]),
            [(0, 100), (110, 110), (100, 1000)]
        );
        assert_eq!(
            regions(18, 18, 10, [5, 5, 5]),
            [(18, 18), (18, 18), (18, 18)]
        );
    }

    #[test]
    fn compositing_keeps_background_opaque() {
        let mut pixel = 0xff000000_u32.to_ne_bytes();
        blend(&mut pixel, Color::rgba(255, 100, 0, 128));
        assert_eq!(u32::from_ne_bytes(pixel), 0xff803200);
        blend(&mut pixel, Color::rgba(255, 255, 255, 0));
        assert_eq!(u32::from_ne_bytes(pixel), 0xff803200);
    }

    #[test]
    fn rejects_wrong_buffer_sizes_without_modifying_pixels() {
        // a bad buffer has to fail before layout, even on a machine with no fonts
        let mut renderer = Renderer {
            fonts: FontSystem::new(),
            cache: SwashCache::new(),
        };
        let geometry = Geometry::new(64, 32, 1).expect("small frame fits");
        let config = velnopt_config::parse("version=1\n[panel]\nedge='top'\nheight=32")
            .expect("valid panel");
        for length in [0, geometry.byte_len() - 1, geometry.byte_len() + 1] {
            let mut pixels = vec![0x42; length];
            assert!(renderer.draw(&mut pixels, geometry, &config, &[]).is_err());
            assert!(pixels.iter().all(|byte| *byte == 0x42));
        }
    }
}

#[cfg(test)]
mod outline_tests {
    use super::*;

    #[test]
    fn focus_outline_is_clipped_and_leaves_the_interior() {
        let geometry = Geometry::new(8, 6, 1).expect("small frame fits");
        let mut pixels = vec![0; geometry.byte_len()];
        Rectangle {
            left: -2,
            right: 20,
            top: 1,
            bottom: 5,
        }
        .outline(&mut pixels, geometry, 1, [255, 0, 0]);
        let at = |x: usize, y: usize| {
            let offset = y * geometry.stride() as usize + x * 4;
            u32::from_ne_bytes(pixels[offset..offset + 4].try_into().unwrap())
        };
        assert_eq!(at(0, 1), 0xffff0000);
        assert_eq!(at(7, 3), 0xffff0000);
        assert_eq!(at(3, 4), 0xffff0000);
        assert_eq!(at(3, 2), 0, "interior stays untouched");
        assert_eq!(at(3, 0), 0, "rows outside the ring stay untouched");
    }

    #[test]
    fn icons_are_validated_scaled_blended_and_clipped() {
        assert!(Image::new(0, 1, &[]).is_none());
        assert!(Image::new(2, 2, &[0; 15]).is_none());
        // left half opaque red, right half transparent
        let rgba = [255, 0, 0, 255, 0, 0, 255, 0];
        let icon = Image::new(2, 1, &rgba).unwrap();
        let geometry = Geometry::new(6, 4, 1).unwrap();
        let mut pixels = vec![0; geometry.byte_len()];
        draw_icon(&mut pixels, geometry, icon, 0, 0, 4, (0, 3));
        let at = |pixels: &[u8], x: usize, y: usize| {
            let offset = y * geometry.stride() as usize + x * 4;
            u32::from_ne_bytes(pixels[offset..offset + 4].try_into().unwrap())
        };
        assert_eq!(at(&pixels, 0, 3), 0xffff0000);
        assert_eq!(at(&pixels, 1, 0), 0xffff0000);
        assert_eq!(
            at(&pixels, 2, 0),
            0xff000000,
            "transparent pixels keep the background"
        );
        assert_eq!(
            at(&pixels, 3, 0),
            0,
            "pixels outside the group stay untouched"
        );
    }
}
