use x11rb::image::{BitsPerPixel, Image, ImageOrder};

pub(super) fn decode(image: &Image<'_>, red_mask: u32, green_mask: u32, blue_mask: u32) -> Vec<u8> {
    const RGB888: (u32, u32, u32) = (0x00ff_0000, 0x0000_ff00, 0x0000_00ff);

    let pixels = usize::from(image.width()) * usize::from(image.height());
    let mut rgba = vec![255; pixels * 4];
    if image.bits_per_pixel() == BitsPerPixel::B32 && (red_mask, green_mask, blue_mask) == RGB888 {
        for (source, target) in image
            .data()
            .chunks_exact(4)
            .take(pixels)
            .zip(rgba.chunks_exact_mut(4))
        {
            match image.byte_order() {
                ImageOrder::LsbFirst => {
                    target[..3].copy_from_slice(&[source[2], source[1], source[0]]);
                }
                ImageOrder::MsbFirst => target[..3].copy_from_slice(&source[1..4]),
            }
        }
        return rgba;
    }

    for y in 0..image.height() {
        for x in 0..image.width() {
            let pixel = image.get_pixel(x, y);
            let offset = (usize::from(y) * usize::from(image.width()) + usize::from(x)) * 4;
            rgba[offset] = channel(pixel, red_mask);
            rgba[offset + 1] = channel(pixel, green_mask);
            rgba[offset + 2] = channel(pixel, blue_mask);
        }
    }
    rgba
}

fn channel(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let value = (pixel & mask) >> mask.trailing_zeros();
    let maximum = mask >> mask.trailing_zeros();
    ((value * 255 + maximum / 2) / maximum) as u8
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use x11rb::image::ScanlinePad;

    use super::*;

    #[test]
    fn rgb888_fast_path_decodes_both_x11_byte_orders() {
        for (order, source) in [
            (ImageOrder::LsbFirst, [3, 2, 1, 0, 30, 20, 10, 0]),
            (ImageOrder::MsbFirst, [0, 1, 2, 3, 0, 10, 20, 30]),
        ] {
            let image = Image::new(
                2,
                1,
                ScanlinePad::Pad32,
                24,
                BitsPerPixel::B32,
                order,
                Cow::Borrowed(&source),
            )
            .expect("forge X11 image");
            assert_eq!(
                decode(&image, 0x00ff_0000, 0x0000_ff00, 0x0000_00ff),
                [1, 2, 3, 255, 10, 20, 30, 255]
            );
        }
    }
}
