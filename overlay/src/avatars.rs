use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

const CDN: &str = "https://cdn.discordapp.com/";

pub fn normalize(url: &str) -> Option<(String, String)> {
    let path = url.strip_prefix(CDN)?;
    let path = path.split(['?', '#']).next()?;
    let (stem, ext) = path.rsplit_once('.')?;
    let word = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if path.len() > 200
        || !["png", "jpg", "jpeg", "webp", "gif"].contains(&ext)
        || !stem.split('/').all(word)
    {
        return None;
    }
    Some((format!("{CDN}{stem}.png?size=64"), stem.replace('/', "_")))
}

pub fn cache_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".cache"),
    };
    Some(base.join("concord-overlay"))
}

fn load(dir: &Path, url: &str) -> Result<Pixmap, String> {
    let (fetch, key) = normalize(url).ok_or("not a Discord CDN image URL")?;
    let file = dir.join(format!("{key}.png"));
    if !file.exists() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let part = dir.join(format!("{key}.png.part"));
        let out = Command::new("curl")
            .args(["--silent", "--show-error", "--fail", "--max-time", "15"])
            .args(["--max-filesize", "1048576", "--proto", "=https", "--output"])
            .arg(&part)
            .arg("--")
            .arg(&fetch)
            .output()
            .map_err(|e| format!("cannot run curl: {e}"))?;
        if !out.status.success() {
            let _ = std::fs::remove_file(&part);
            return Err(format!(
                "curl {fetch}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        std::fs::rename(&part, &file).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    let data = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let decoded = match png_size(&data) {
        Some((w, h)) if w > 1024 || h > 1024 => Err(format!("image is {w}x{h}, over 1024 px")),
        _ => Pixmap::decode_png(&data).map_err(|e| e.to_string()),
    };
    let img = decoded.map_err(|e| {
        let _ = std::fs::remove_file(&file);
        format!("{}: {e}", file.display())
    })?;
    Ok(shrink(img))
}

fn png_size(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 24 || &data[12..16] != b"IHDR" {
        return None;
    }
    let be = |i: usize| u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
    Some((be(16), be(20)))
}

fn shrink(mut img: Pixmap) -> Pixmap {
    while img.width() > 64 && img.height() > 64 {
        let mut half = Pixmap::new(img.width().div_ceil(2), img.height().div_ceil(2)).unwrap();
        let p = PixmapPaint {
            quality: FilterQuality::Bilinear,
            ..Default::default()
        };
        half.draw_pixmap(
            0,
            0,
            img.as_ref(),
            &p,
            Transform::from_scale(0.5, 0.5),
            None,
        );
        img = half;
    }
    img
}

pub type Loaded = (String, Result<Pixmap, String>);

pub fn spawn(dir: PathBuf, done: impl Fn(Loaded) + Send + 'static) -> mpsc::Sender<String> {
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for url in rx {
            let result = load(&dir, &url);
            done((url, result));
        }
    });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_cdn_image_urls_pass() {
        assert_eq!(
            normalize("https://cdn.discordapp.com/avatars/123/a_0123abcd.gif?size=1024"),
            Some((
                "https://cdn.discordapp.com/avatars/123/a_0123abcd.png?size=64".into(),
                "avatars_123_a_0123abcd".into()
            ))
        );
        assert_eq!(
            normalize("https://cdn.discordapp.com/embed/avatars/3.png")
                .unwrap()
                .1,
            "embed_avatars_3"
        );
        assert!(normalize("https://cdn.discordapp.com/guilds/1/users/2/avatars/ff.webp").is_some());
        for bad in [
            "http://cdn.discordapp.com/avatars/1/a.png",
            "https://cdn.discordapp.com.evil.example/avatars/1/a.png",
            "https://evil.example/https://cdn.discordapp.com/avatars/1/a.png",
            "https://cdn.discordapp.com/avatars/../../etc/passwd.png",
            "https://cdn.discordapp.com/avatars/1/a b.png",
            "https://cdn.discordapp.com/avatars/1/a.svg",
            "https://cdn.discordapp.com//a.png",
        ] {
            assert_eq!(normalize(bad), None, "{bad}");
        }
    }

    #[test]
    fn png_size_reads_ihdr() {
        let png = Pixmap::new(3, 2).unwrap().encode_png().unwrap();
        assert_eq!(png_size(&png), Some((3, 2)));
        assert_eq!(png_size(b"not a png"), None);
    }

    #[test]
    fn oversized_cached_png_is_rejected_and_deleted() {
        let dir = std::env::temp_dir().join(format!("concord-overlay-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("embed_avatars_3.png");
        let mut png = Pixmap::new(1, 1).unwrap().encode_png().unwrap();
        png[20..24].copy_from_slice(&2000u32.to_be_bytes());
        std::fs::write(&file, &png).unwrap();
        let err = load(&dir, "https://cdn.discordapp.com/embed/avatars/3.png").unwrap_err();
        assert!(err.contains("1x2000, over 1024 px"), "{err}");
        assert!(!file.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn shrink_halves_large_images() {
        let img = shrink(Pixmap::new(256, 256).unwrap());
        assert_eq!((img.width(), img.height()), (64, 64));
        let img = shrink(Pixmap::new(64, 64).unwrap());
        assert_eq!(img.width(), 64);
    }
}
