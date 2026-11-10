//! Text clips drawn: a title's words as a frame-sized picture with a see-through plane, the same
//! for the viewer and the film.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use cannotcut_timeline::{Title, TitleFont};

/// The faces titles are set in, shipped with the editor, which uses them for its own words too.
pub fn font_file(font: TitleFont) -> &'static [u8] {
    match font {
        TitleFont::Oswald => include_bytes!("../../cannotcut-app/fonts/Oswald-Regular.ttf"),
        TitleFont::Antonio => include_bytes!("../../cannotcut-app/fonts/Antonio-Regular.ttf"),
        TitleFont::AntonSc => include_bytes!("../../cannotcut-app/fonts/AntonSC-Regular.ttf"),
    }
}

/// A title drawn as 4:2:0 planes, BT.709 limited range: luma, a (Cb, Cr) pair per two by two
/// pixels, and how much of each pixel the words cover, 0 to 255. Rows are packed.
pub struct TitlePicture {
    pub width: u32,
    pub height: u32,
    pub luma: Vec<u8>,
    pub chroma: Vec<u8>,
    pub alpha: Vec<u8>,
}

/// Draws `title` on a see-through picture of `size`, the frame's shape, its lines centred
/// across and the block of them centred up and down. Sides are rounded down to even numbers.
pub fn draw_title(title: &Title, size: (u32, u32)) -> TitlePicture {
    let width = (size.0 & !1).max(2);
    let height = (size.1 & !1).max(2);
    let mut cover = vec![0.0f32; width as usize * height as usize];
    let font = FontRef::try_from_slice(font_file(title.font)).expect("a shipped font reads");
    let scaled = font.as_scaled(PxScale::from(title.size * height as f32));
    let lines: Vec<&str> = title.text.split('\n').collect();
    let line_height = scaled.height() + scaled.line_gap();
    let block = line_height * lines.len() as f32 - scaled.line_gap();
    let mut baseline = (height as f32 - block) / 2.0 + scaled.ascent();
    for line in lines {
        let line = line.trim_end_matches('\r');
        // Laid out once to measure, then again where it goes.
        let mut glyphs = Vec::new();
        let mut caret = 0.0;
        let mut previous = None;
        for character in line.chars() {
            let id = font.glyph_id(character);
            if let Some(previous) = previous {
                caret += scaled.kern(previous, id);
            }
            glyphs.push((id, caret));
            caret += scaled.h_advance(id);
            previous = Some(id);
        }
        let left = (width as f32 - caret) / 2.0;
        for (id, x) in glyphs {
            let glyph = id.with_scale_and_position(scaled.scale(), point(left + x, baseline));
            let Some(outlined) = font.outline_glyph(glyph) else {
                continue;
            };
            let bounds = outlined.px_bounds();
            outlined.draw(|x, y, coverage| {
                let x = bounds.min.x as i64 + i64::from(x);
                let y = bounds.min.y as i64 + i64::from(y);
                if (0..i64::from(width)).contains(&x) && (0..i64::from(height)).contains(&y) {
                    let at = y as usize * width as usize + x as usize;
                    // Where glyphs overlap, the more covered wins: no seams.
                    cover[at] = cover[at].max(coverage);
                }
            });
        }
        baseline += line_height;
    }

    let [red, green, blue, opacity] = title.color;
    let (y, cb, cr) = to_yuv([red, green, blue]);
    let alpha = cover
        .iter()
        .map(|coverage| (coverage.clamp(0.0, 1.0) * f32::from(opacity)).round() as u8)
        .collect();
    let pixels = width as usize * height as usize;
    TitlePicture {
        width,
        height,
        luma: vec![y; pixels],
        chroma: [cb, cr].repeat(pixels / 4),
        alpha,
    }
}

/// A gamma-encoded colour as BT.709 limited-range samples.
fn to_yuv([red, green, blue]: [u8; 3]) -> (u8, u8, u8) {
    let [red, green, blue] = [red, green, blue].map(|value| f32::from(value) / 255.0);
    let luma = 0.2126 * red + 0.7152 * green + 0.0722 * blue;
    let blue_difference = (blue - luma) / 1.8556;
    let red_difference = (red - luma) / 1.5748;
    let code = |value: f32| value.round().clamp(0.0, 255.0) as u8;
    (
        code(16.0 + 219.0 * luma),
        code(128.0 + 224.0 * blue_difference),
        code(128.0 + 224.0 * red_difference),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_drawn_in_the_middle_of_the_frame() {
        let picture = draw_title(&Title::new("HI"), (320, 180));
        assert_eq!((picture.width, picture.height), (320, 180));
        assert_eq!(picture.alpha.len(), 320 * 180);
        assert_eq!(picture.chroma.len(), 320 * 180 / 2);
        let covered = |x: usize, y: usize| picture.alpha[y * 320 + x];
        // Nothing at the corners, something across the middle.
        assert_eq!(covered(0, 0), 0);
        assert_eq!(covered(319, 179), 0);
        assert!((140..180).any(|x| covered(x, 90) == 255));
        // White, in limited range.
        assert_eq!((picture.luma[0], picture.chroma[0], picture.chroma[1]), (235, 128, 128));
    }

    #[test]
    fn several_lines_stack_up() {
        let one = draw_title(&Title::new("A"), (200, 400));
        let two = draw_title(&Title::new("A\nA"), (200, 400));
        let rows = |picture: &TitlePicture| {
            (0..400)
                .filter(|row| picture.alpha[row * 200..(row + 1) * 200].iter().any(|a| *a > 0))
                .count()
        };
        assert!(rows(&two) > rows(&one) * 2 - 4, "{} {}", rows(&one), rows(&two));
    }

    #[test]
    fn colours_become_limited_range_samples() {
        assert_eq!(to_yuv([0, 0, 0]), (16, 128, 128));
        assert_eq!(to_yuv([255, 0, 0]), (63, 102, 240));
    }
}
