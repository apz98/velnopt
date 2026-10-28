//! Tray icons: ARGB pixmaps from the item, or PNG files from the icon directories.
//!
//! SVG isn't decoded. Those icons give `None`, and the widget shows the title.

use std::{
    env,
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
    sync::Arc,
};

use png::{ColorType, Decoder, Limits, Transformations};
use rustix::fs::{Mode, OFlags};

use crate::Icon;

/// The size a panel icon is drawn at.
const WANTED: i32 = 32;
const MAX_SIDE: i32 = 256;
const MAX_PNG_BYTES: u64 = 1024 * 1024;
const MAX_NAME: usize = 128;
const SIZES: [&str; 8] = [
    "32x32", "48x48", "24x24", "64x64", "22x22", "16x16", "128x128", "256x256",
];

/// Pick the pixmap closest to 32 px (the larger one wins a tie) and convert it.
///
/// Pixmaps are ignored if a side is zero, negative, or over 256, or if the data
/// length isn't `width * height * 4`.
pub(crate) fn from_pixmaps(pixmaps: Vec<(i32, i32, Vec<u8>)>) -> Option<Icon> {
    let (width, height, argb) = pixmaps
        .into_iter()
        .filter(|(width, height, data)| {
            (1..=MAX_SIDE).contains(width)
                && (1..=MAX_SIDE).contains(height)
                && data.len() == *width as usize * *height as usize * 4
        })
        .min_by_key(|(width, height, _)| {
            let side = *width.max(height);
            ((side - WANTED).abs(), -side)
        })?;
    // network byte order ARGB32 to RGBA8, with no alpha conversion
    let rgba = argb
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[alpha, red, green, blue]| [red, green, blue, alpha])
        .collect();
    Some(Icon {
        width: width as u16,
        height: height as u16,
        rgba,
    })
}

/// Find `name` as a PNG in `theme_path` or the icon trees of the data directories.
///
/// Names with path separators are refused, so an item can't make the panel read
/// files outside the icon directories.
pub(crate) fn from_theme(name: &str, theme_path: &str) -> Option<Icon> {
    if name.is_empty()
        || name.len() > MAX_NAME
        || name.starts_with('.')
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
    {
        return None;
    }
    let file = if name.ends_with(".png") {
        name.to_owned()
    } else {
        format!("{name}.png")
    };
    let mut roots = Vec::new();
    if Path::new(theme_path).is_absolute() {
        roots.push(PathBuf::from(theme_path));
    }
    roots.extend(data_directories().into_iter().map(|dir| dir.join("icons")));
    roots
        .iter()
        .flat_map(|root| {
            // the theme path can be a flat directory or a theme root
            let flat = std::iter::once(root.join(&file));
            let themed = SIZES
                .iter()
                .map(|size| root.join("hicolor").join(size).join("apps").join(&file));
            flat.chain(themed).collect::<Vec<_>>()
        })
        .chain(std::iter::once(Path::new("/usr/share/pixmaps").join(&file)))
        .find_map(|path| decode_png(&path))
}

fn data_directories() -> Vec<PathBuf> {
    let home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")));
    let system = env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    home.into_iter()
        .chain(system.split(':').map(PathBuf::from))
        .filter(|dir| dir.is_absolute())
        .take(16)
        .collect()
}

/// Decode a regular PNG file (at most 1 MiB and 256x256 pixels) to RGBA8.
pub(crate) fn decode_png(path: &Path) -> Option<Icon> {
    // the path comes from any session-bus client: opening a FIFO without NONBLOCK
    // would hang the tray worker before the regular-file check below
    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = File::from(rustix::fs::open(path, flags, Mode::empty()).ok()?);
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_PNG_BYTES {
        return None;
    }
    let mut decoder = Decoder::new_with_limits(
        BufReader::new(file),
        Limits {
            bytes: 4 * 1024 * 1024,
        },
    );
    decoder.set_transformations(Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = (reader.info().width, reader.info().height);
    if width == 0 || height == 0 || width > MAX_SIDE as u32 || height > MAX_SIDE as u32 {
        return None;
    }
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut pixels).ok()?;
    let pixels = &pixels[..frame.buffer_size()];
    let rgba: Arc<[u8]> = match frame.color_type {
        ColorType::Rgba => pixels.into(),
        ColorType::Rgb => pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[red, green, blue]| [red, green, blue, 255])
            .collect(),
        ColorType::GrayscaleAlpha => pixels
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&[gray, alpha]| [gray, gray, gray, alpha])
            .collect(),
        ColorType::Grayscale => pixels.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        ColorType::Indexed => return None,
    };
    (rgba.len() == width as usize * height as usize * 4).then_some(Icon {
        width: width as u16,
        height: height as u16,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::tests::tray::png_bytes;

    fn argb(pixels: usize) -> Vec<u8> {
        [255, 1, 2, 3].repeat(pixels)
    }

    #[test]
    fn closest_size_wins_and_ties_prefer_larger() {
        let pick = |sizes: &[i32]| {
            from_pixmaps(
                sizes
                    .iter()
                    .map(|&side| (side, side, argb((side * side) as usize)))
                    .collect(),
            )
            .map(|icon| icon.width)
        };
        assert_eq!(pick(&[16, 48, 31]), Some(31));
        assert_eq!(pick(&[24, 40]), Some(40));
        assert_eq!(pick(&[]), None);
    }

    #[test]
    fn bad_pixmaps_are_rejected() {
        assert!(from_pixmaps(vec![(2, 2, vec![0; 15])]).is_none(), "short");
        assert!(from_pixmaps(vec![(2, 2, vec![0; 17])]).is_none(), "long");
        assert!(from_pixmaps(vec![(0, 4, vec![])]).is_none(), "empty side");
        assert!(
            from_pixmaps(vec![(-1, -1, vec![0; 4])]).is_none(),
            "negative"
        );
        let big = 300_usize;
        assert!(
            from_pixmaps(vec![(300, 300, vec![0; big * big * 4])]).is_none(),
            "over 256"
        );
        assert!(
            from_pixmaps(vec![(i32::MAX, i32::MAX, vec![0; 4])]).is_none(),
            "overflow"
        );
        // a valid one next to a bad one still wins
        assert!(from_pixmaps(vec![(2, 2, vec![0; 3]), (1, 1, argb(1))]).is_some());
    }

    #[test]
    fn theme_names_cannot_escape_icon_directories() {
        for name in [
            "../secret",
            "a/b",
            "/etc/passwd",
            ".hidden",
            "",
            "a b",
            &"x".repeat(200),
        ] {
            assert!(from_theme(name, "/tmp").is_none(), "{name:?}");
        }
    }

    #[test]
    fn png_conversion_and_bounds() {
        let folder = std::env::temp_dir().join(format!("vsi-{}", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        let write = |name: &str, bytes: Vec<u8>| {
            let path = folder.join(name);
            fs::write(&path, bytes).unwrap();
            path
        };
        let gray = decode_png(&write(
            "g.png",
            png_bytes(1, 2, ColorType::Grayscale, &[7, 9]),
        ))
        .unwrap();
        assert_eq!(&*gray.rgba, &[7, 7, 7, 255, 9, 9, 9, 255]);
        let gray_alpha = decode_png(&write(
            "ga.png",
            png_bytes(1, 1, ColorType::GrayscaleAlpha, &[5, 128]),
        ))
        .unwrap();
        assert_eq!(&*gray_alpha.rgba, &[5, 5, 5, 128]);
        let rgb = decode_png(&write(
            "rgb.png",
            png_bytes(1, 1, ColorType::Rgb, &[1, 2, 3]),
        ))
        .unwrap();
        assert_eq!(&*rgb.rgba, &[1, 2, 3, 255]);
        let wide = write(
            "wide.png",
            png_bytes(257, 1, ColorType::Grayscale, &[0; 257]),
        );
        assert!(decode_png(&wide).is_none(), "257 px wide");
        assert!(decode_png(&write("junk.png", b"not a png".to_vec())).is_none());
        assert!(decode_png(&folder).is_none(), "directories are not icons");
        assert!(decode_png(&folder.join("absent.png")).is_none());
        let _ = fs::remove_dir_all(&folder);
    }

    #[test]
    fn fifos_are_rejected_without_blocking() {
        let folder = std::env::temp_dir().join(format!("vsf-{}", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        let fifo = folder.join("icon.png");
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).unwrap();
        // a blocking open would never return, so decode on a thread and time out
        let (done, result) = std::sync::mpsc::channel();
        let path = fifo.clone();
        std::thread::spawn(move || done.send(decode_png(&path).is_none()));
        let rejected = result.recv_timeout(std::time::Duration::from_secs(5));
        let _ = fs::remove_dir_all(&folder);
        assert_eq!(rejected, Ok(true), "a FIFO icon path must fail fast");
    }
}
