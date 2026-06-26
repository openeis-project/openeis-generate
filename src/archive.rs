//! Compressed-archive template sources (zip / tar.gz).
//!
//! A template can be supplied as a local archive file or an HTTP(S) URL. The
//! archive is extracted into a temp dir, which then feeds the normal `expand`
//! pipeline.

use std::fs::File;
use std::io;
use std::path::{Component, Path};

use anyhow::{Context, Result};

/// Supported archive formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    TarGz,
}

/// Detect a format from a filename or URL by its extension.
pub fn detect(name: &str) -> Option<Format> {
    let lower = name.to_ascii_lowercase();
    let lower = lower.split('?').next().unwrap_or(&lower); // strip query string
    if lower.ends_with(".zip") {
        Some(Format::Zip)
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        Some(Format::TarGz)
    } else {
        None
    }
}

/// True if `s` looks like an HTTP(S) URL.
pub fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Download `url` into `dest` (follows redirects).
pub fn download(url: &str, dest: &Path) -> Result<()> {
    let resp = ureq::get(url)
        .call()
        .with_context(|| format!("downloading {url}"))?;
    let mut file = File::create(dest)
        .with_context(|| format!("creating {}", dest.display()))?;
    let mut body = resp.into_reader();
    io::copy(&mut body, &mut file)?;
    Ok(())
}

/// Extract `archive` into `dest`.
pub fn extract(archive: &Path, dest: &Path, fmt: Format) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    match fmt {
        Format::Zip => extract_zip(archive, dest),
        Format::TarGz => extract_targz(archive, dest),
    }
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().to_string();
        // Guard against zip-slip: reject absolute paths or any `..` component.
        let safe = Path::new(&name)
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
        if !safe {
            continue;
        }

        let outpath = dest.join(&name);
        // Encoded directory entry.
        if name.ends_with('/') || entry.is_dir() {
            std::fs::create_dir_all(&outpath)?;
            continue;
        }
        if let Some(parent) = outpath.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = File::create(&outpath)?;
        io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

fn extract_targz(archive: &Path, dest: &Path) -> Result<()> {
    let file = File::open(archive)?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    // `unpack` sanitizes entries against path traversal.
    tar.unpack(dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn detect_by_extension() {
        assert_eq!(detect("t.zip"), Some(Format::Zip));
        assert_eq!(detect("t.ZIP"), Some(Format::Zip));
        assert_eq!(detect("t.tar.gz"), Some(Format::TarGz));
        assert_eq!(detect("t.tgz"), Some(Format::TarGz));
        assert_eq!(detect("t.TAR.GZ"), Some(Format::TarGz));
        // query string is stripped (URLs)
        assert_eq!(detect("https://x/y/t.zip?token=abc"), Some(Format::Zip));
        assert_eq!(detect("t.tar"), None);
        assert_eq!(detect("t.bin"), None);
    }

    #[test]
    fn url_detection() {
        assert!(is_url("https://example.com/t.zip"));
        assert!(is_url("http://x/t.tar.gz"));
        assert!(!is_url("/tmp/t.zip"));
        assert!(!is_url("t.zip"));
    }

    fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let f = File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    fn make_targz(path: &Path, entries: &[(&str, &[u8])]) {
        let f = File::create(path).unwrap();
        let enc = GzEncoder::new(f, Compression::default());
        let mut b = tar::Builder::new(enc);
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            b.append_data(&mut header, name, std::io::Cursor::new(*data))
                .unwrap();
        }
        b.finish().unwrap();
    }

    #[test]
    fn extract_zip_roundtrip() {
        let arc = TempDir::new().unwrap();
        let zip_path = arc.path().join("t.zip");
        make_zip(
            &zip_path,
            &[
                ("README.md", b"# hi {{ name }}"),
                ("src/main.rs", b"fn main() {}"),
            ],
        );

        let out = TempDir::new().unwrap();
        extract(&zip_path, out.path(), Format::Zip).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.path().join("README.md")).unwrap(),
            "# hi {{ name }}"
        );
        assert_eq!(
            std::fs::read_to_string(out.path().join("src/main.rs")).unwrap(),
            "fn main() {}"
        );
    }

    #[test]
    fn extract_targz_roundtrip() {
        let arc = TempDir::new().unwrap();
        let tgz_path = arc.path().join("t.tar.gz");
        make_targz(
            &tgz_path,
            &[
                ("README.md", b"# hi"),
                ("src/lib.rs", b"pub fn x() {}"),
            ],
        );

        let out = TempDir::new().unwrap();
        extract(&tgz_path, out.path(), Format::TarGz).unwrap();
        assert_eq!(std::fs::read_to_string(out.path().join("README.md")).unwrap(), "# hi");
        assert_eq!(
            std::fs::read_to_string(out.path().join("src/lib.rs")).unwrap(),
            "pub fn x() {}"
        );
    }

    #[test]
    fn extract_zip_rejects_path_traversal() {
        let arc = TempDir::new().unwrap();
        let zip_path = arc.path().join("evil.zip");
        make_zip(&zip_path, &[("../escape.txt", b"pwned")]);

        let out = TempDir::new().unwrap();
        extract(&zip_path, out.path(), Format::Zip).unwrap();
        // traversal entry must be skipped, nothing written outside dest
        assert!(!arc.path().join("escape.txt").exists());
        assert!(out.path().read_dir().unwrap().next().is_none());
    }
}
