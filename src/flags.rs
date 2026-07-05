//! Country flags, sliced out of an atlas compiled into the binary.
//!
//! Windows will not draw flag emoji: Segoe UI Emoji carries no glyphs for the
//! regional indicator pairs and renders the two letters instead, deliberately.
//! So the flags are images, and they are embedded rather than shipped as files:
//! `ui/assets/flags.bin` is one strip of raw RGBA, 32x24 per country, and a row
//! of it goes straight to Slint with no decoding and nothing on disk.
//!
//! Built from the MIT-licensed flag-icons set — see vendor/flag-icons/LICENSE.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;

const WIDTH: u32 = 32;
const HEIGHT: u32 = 24;
const STRIDE: usize = (WIDTH * HEIGHT * 4) as usize;

static ATLAS: &[u8] = include_bytes!("../ui/assets/flags.bin");
/// The same countries as the atlas, two ASCII bytes each, in the same order and
/// sorted — which is what makes the lookup a binary search over fixed records.
static INDEX: &[u8] = include_bytes!("../ui/assets/flags.idx");

thread_local! {
    /// A `slint::Image` is refcounted, so caching one per country keeps the list
    /// from rebuilding the same pixels every time the model is republished.
    static CACHE: RefCell<HashMap<usize, Image>> = RefCell::new(HashMap::new());
}

pub fn count() -> usize {
    INDEX.len() / 2
}

fn position(code: &str) -> Option<usize> {
    let code = code.trim().to_ascii_lowercase();
    let key = code.as_bytes();
    if key.len() != 2 {
        return None;
    }

    let (mut low, mut high) = (0usize, count());
    while low < high {
        let middle = (low + high) / 2;
        match INDEX[middle * 2..middle * 2 + 2].cmp(key) {
            Ordering::Less => low = middle + 1,
            Ordering::Greater => high = middle,
            Ordering::Equal => return Some(middle),
        }
    }
    None
}

/// The flag for an ISO 3166-1 alpha-2 code. An unknown code gives an empty
/// image, which draws nothing and leaves the country name to carry the row.
pub fn flag(code: &str) -> Image {
    let Some(at) = position(code) else {
        return Image::default();
    };
    CACHE.with(|cache| {
        cache
            .borrow_mut()
            .entry(at)
            .or_insert_with(|| slice(at))
            .clone()
    })
}

pub fn known(code: &str) -> bool {
    position(code).is_some()
}

fn slice(at: usize) -> Image {
    let start = at * STRIDE;
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(WIDTH, HEIGHT);
    for (offset, pixel) in buffer.make_mut_slice().iter_mut().enumerate() {
        let byte = start + offset * 4;
        *pixel = Rgba8Pixel {
            r: ATLAS[byte],
            g: ATLAS[byte + 1],
            b: ATLAS[byte + 2],
            a: ATLAS[byte + 3],
        };
    }
    Image::from_rgba8(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_and_index_agree() {
        assert!(count() > 200, "expected the full ISO-2 set, got {}", count());
        assert_eq!(ATLAS.len(), count() * STRIDE);
    }

    #[test]
    fn index_is_sorted_so_the_search_holds() {
        for pair in (0..count()).collect::<Vec<_>>().windows(2) {
            let (before, after) = (pair[0] * 2, pair[1] * 2);
            assert!(INDEX[before..before + 2] < INDEX[after..after + 2]);
        }
    }

    #[test]
    fn finds_countries_and_refuses_the_rest() {
        for code in ["fr", "de", "is", "jp", "us", "za"] {
            assert!(known(code), "{code} should have a flag");
        }
        // Case and padding are what the control plane might actually send.
        assert!(known("FR"));
        assert!(known(" fr "));
        // Not countries.
        assert!(!known("zz"));
        assert!(!known("f"));
        assert!(!known("fra"));
        assert!(!known(""));
    }

    #[test]
    fn a_flag_has_the_expected_shape() {
        let image = flag("fr");
        assert_eq!(image.size().width, WIDTH);
        assert_eq!(image.size().height, HEIGHT);
        // The French flag is opaque, and its left column is blue.
        let pixels = image.to_rgba8().unwrap();
        let first = pixels.as_slice()[0];
        assert_eq!(first.a, 255);
        assert!(first.b > first.r, "the hoist should be blue, got {first:?}");
    }

    #[test]
    fn an_unknown_code_gives_an_empty_image() {
        assert_eq!(flag("zz").size().width, 0);
    }
}
