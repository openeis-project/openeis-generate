//! Compressed-archive template sources and packaging (`zip` / `tar.gz` / `tar.zst`).
//!
//! A template can be supplied as a local archive file or an HTTP(S) URL; the
//! archive is extracted into a temp dir that feeds the normal `expand` pipeline
//! (the read/extract path). The [`pack`] function goes the other way — bundling
//! a template directory into a distributable archive for the `package`
//! subcommand (and, later, `publish`).
//!
//! `.tar.zst` uses the `zstd` crate for both encode and decode; `zstd-sys` is
//! already compiled transitively via `zip`, so this adds no new native build.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use globset::GlobSet;
use walkdir::WalkDir;

/// Supported archive formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    Zip,
    #[value(name = "tar-gz", alias = "tgz")]
    TarGz,
    #[value(name = "tar-zst", alias = "tzst")]
    TarZst,
}

impl Format {
    /// File extension(s) for this format, the canonical one first.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Format::Zip => &["zip"],
            Format::TarGz => &["tar.gz", "tgz"],
            Format::TarZst => &["tar.zst", "tzst"],
        }
    }

    /// The canonical extension (e.g. `.tar.zst`) used when deriving an output
    /// name from a template directory.
    pub fn primary_extension(self) -> &'static str {
        self.extensions()[0]
    }
}

/// Detect a format from a filename or URL by its extension.
pub fn detect(name: &str) -> Option<Format> {
    let lower = name.to_ascii_lowercase();
    let lower = lower.split('?').next().unwrap_or(&lower); // strip query string
    if lower.ends_with(".zip") {
        Some(Format::Zip)
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        Some(Format::TarGz)
    } else if lower.ends_with(".tar.zst") || lower.ends_with(".tzst") {
        Some(Format::TarZst)
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
        Format::TarZst => extract_tarzst(archive, dest),
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

fn extract_tarzst(archive: &Path, dest: &Path) -> Result<()> {
    let file = File::open(archive)?;
    let zst = zstd::Decoder::new(io::BufReader::new(file))?;
    let mut tar = tar::Archive::new(zst);
    tar.unpack(dest)?;
    Ok(())
}

// ── packaging ───────────────────────────────────────────────────────────────

/// Paths that are never packaged out of a template (VCS metadata).
const NEVER_PACK: &[&str] = &[".git"];

/// Aggregate counts from a [`pack`] run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PackStats {
    pub dirs: usize,
    pub files: usize,
    pub bytes: u64,
}

/// Bundle `src_dir` into `dest` as `fmt`. Files are copied **verbatim** (no
/// Liquid rendering) — a packaged template ships raw so it can be regenerated.
/// `template.kdl` and `.genignore` are included; `.git` is always excluded, and
/// any path matching `ignore` (built from the template's `.genignore`) is
/// dropped — including the descent into matched directories.
pub fn pack(
    src_dir: &Path,
    dest: &Path,
    fmt: Format,
    level: Option<i32>,
    ignore: &GlobSet,
) -> Result<PackStats> {
    if !src_dir.is_dir() {
        anyhow::bail!("template path is not a directory: {}", src_dir.display());
    }
    if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }

    // Collect the entries to keep (relative paths). filter_entry prunes
    // never-pack / ignored directories so we don't descend into them; it also
    // drops ignored files, so everything yielded here is kept verbatim.
    let mut stats = PackStats::default();
    let mut entries: Vec<(PathBuf, bool)> = Vec::new();
    for entry in WalkDir::new(src_dir)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| keep_entry(e.path(), src_dir, ignore))
    {
        let entry = entry?;
        let rel = entry.path().strip_prefix(src_dir)?.to_path_buf();
        let is_dir = entry.file_type().is_dir();
        if is_dir {
            stats.dirs += 1;
        } else {
            stats.files += 1;
            stats.bytes += entry.metadata()?.len();
        }
        entries.push((rel, is_dir));
    }

    let file = File::create(dest)
        .with_context(|| format!("creating {}", dest.display()))?;
    match fmt {
        Format::Zip => pack_zip(file, src_dir, &entries)?,
        Format::TarGz => pack_targz(file, src_dir, &entries, level.unwrap_or(6) as u32)?,
        Format::TarZst => pack_tarzst(file, src_dir, &entries, level.unwrap_or(3))?,
    }
    Ok(stats)
}

/// filter_entry predicate: keep a file unless it matches the ignore set, and
/// prune a directory if it is never-pack or matches the ignore set (so the walk
/// does not descend into it).
fn keep_entry(path: &Path, root: &Path, ignore: &GlobSet) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return true;
    };
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    if path.is_dir() {
        !(never_pack(&rel_str) || ignore.is_match(&rel_str))
    } else {
        !ignore.is_match(&rel_str)
    }
}

fn never_pack(rel: &str) -> bool {
    NEVER_PACK
        .iter()
        .any(|n| rel == *n || rel.starts_with(&format!("{n}/")))
}

fn pack_zip(file: File, src_dir: &Path, entries: &[(PathBuf, bool)]) -> Result<()> {
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    for (rel, is_dir) in entries {
        let name = rel.to_string_lossy().replace('\\', "/");
        if *is_dir {
            zip.add_directory(&name, opts)
                .with_context(|| format!("adding dir {name} to zip"))?;
        } else {
            zip.start_file(&name, opts)
                .with_context(|| format!("adding {name} to zip"))?;
            let bytes = std::fs::read(src_dir.join(rel))?;
            zip.write_all(&bytes)?;
        }
    }
    zip.finish()?;
    Ok(())
}

/// Append the kept entries to a tar builder (files, dirs, and symlinks via
/// `append_path_with_name`, which reads size/mode/mtime from the filesystem).
fn append_tar<W: Write>(
    tar: &mut tar::Builder<W>,
    src_dir: &Path,
    entries: &[(PathBuf, bool)],
) -> Result<()> {
    for (rel, _) in entries {
        tar.append_path_with_name(src_dir.join(rel), rel)
            .with_context(|| format!("adding {} to tar", rel.display()))?;
    }
    Ok(())
}

fn pack_targz(
    file: File,
    src_dir: &Path,
    entries: &[(PathBuf, bool)],
    level: u32,
) -> Result<()> {
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::new(level));
    let mut tar = tar::Builder::new(enc);
    append_tar(&mut tar, src_dir, entries)?;
    tar.finish()?;
    let enc = tar.into_inner()?;
    enc.finish()?;
    Ok(())
}

fn pack_tarzst(
    file: File,
    src_dir: &Path,
    entries: &[(PathBuf, bool)],
    level: i32,
) -> Result<()> {
    let enc = zstd::stream::write::Encoder::new(file, level)?;
    let mut tar = tar::Builder::new(enc);
    append_tar(&mut tar, src_dir, entries)?;
    tar.finish()?;
    let enc = tar.into_inner()?;
    enc.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn detect_by_extension() {
        assert_eq!(detect("t.zip"), Some(Format::Zip));
        assert_eq!(detect("t.ZIP"), Some(Format::Zip));
        assert_eq!(detect("t.tar.gz"), Some(Format::TarGz));
        assert_eq!(detect("t.tgz"), Some(Format::TarGz));
        assert_eq!(detect("t.TAR.GZ"), Some(Format::TarGz));
        assert_eq!(detect("t.tar.zst"), Some(Format::TarZst));
        assert_eq!(detect("t.tzst"), Some(Format::TarZst));
        assert_eq!(detect("t.TAR.ZST"), Some(Format::TarZst));
        // query string is stripped (URLs)
        assert_eq!(detect("https://x/y/t.zip?token=abc"), Some(Format::Zip));
        assert_eq!(detect("https://x/y/t.tar.zst"), Some(Format::TarZst));
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

    fn make_tarzst(path: &Path, entries: &[(&str, &[u8])]) {
        let f = File::create(path).unwrap();
        let enc = zstd::stream::write::Encoder::new(f, 3).unwrap();
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
        b.into_inner().unwrap().finish().unwrap();
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
        assert_eq!(
            std::fs::read_to_string(out.path().join("README.md")).unwrap(),
            "# hi"
        );
        assert_eq!(
            std::fs::read_to_string(out.path().join("src/lib.rs")).unwrap(),
            "pub fn x() {}"
        );
    }

    #[test]
    fn extract_tarzst_roundtrip() {
        let arc = TempDir::new().unwrap();
        let zst_path = arc.path().join("t.tar.zst");
        make_tarzst(
            &zst_path,
            &[
                ("README.md", b"# zst {{ name }}"),
                ("src/lib.rs", b"pub fn z() {}"),
            ],
        );

        let out = TempDir::new().unwrap();
        extract(&zst_path, out.path(), Format::TarZst).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.path().join("README.md")).unwrap(),
            "# zst {{ name }}"
        );
        assert_eq!(
            std::fs::read_to_string(out.path().join("src/lib.rs")).unwrap(),
            "pub fn z() {}"
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

    // ── pack ──

    fn ignore_set(patterns: &[&str]) -> GlobSet {
        let mut b = globset::GlobSetBuilder::new();
        for p in patterns {
            b.add(globset::Glob::new(p).unwrap());
        }
        b.build().unwrap()
    }

    /// A representative raw template tree: a config, a liquid file, a nested
    /// source file, plus a `.genignore`, a secret it should drop, and `.git`.
    fn make_template_tree(root: &Path) {
        let at = |rel: &str| {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            p
        };
        fs::write(at("template.kdl"), "template { }\n").unwrap();
        fs::write(at("README.md.liquid"), "# {{ name }}\n").unwrap();
        fs::write(at("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(at(crate::generate::IGNORE_FILE_NAME), "secrets.env\n").unwrap();
        fs::write(at("secrets.env"), "TOKEN=shh\n").unwrap();
        fs::write(at(".git/config"), "[core]\n").unwrap();
    }

    /// Assert the packaging invariants on an extracted tree: template.kdl +
    /// raw liquid + nested file present and unrendered, secret + .git dropped,
    /// .genignore kept.
    fn assert_packaged_template(extracted: &Path) {
        assert_eq!(
            fs::read_to_string(extracted.join("template.kdl")).unwrap(),
            "template { }\n",
            "template.kdl must be packaged"
        );
        // Liquid untouched (raw template, no rendering).
        assert_eq!(
            fs::read_to_string(extracted.join("README.md.liquid")).unwrap(),
            "# {{ name }}\n",
            "packaging must NOT render liquid"
        );
        assert_eq!(
            fs::read_to_string(extracted.join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
        assert!(
            extracted.join(crate::generate::IGNORE_FILE_NAME).exists(),
            ".genignore itself is kept"
        );
        assert!(!extracted.join("secrets.env").exists(), "ignored secret dropped");
        assert!(!extracted.join(".git").exists(), ".git never packaged");
    }

    #[test]
    fn pack_zip_roundtrip() {
        let src = TempDir::new().unwrap();
        make_template_tree(src.path());
        let arc = TempDir::new().unwrap();
        let out_path = arc.path().join("pkg.zip");

        let stats = pack(
            src.path(),
            &out_path,
            Format::Zip,
            None,
            &ignore_set(&["secrets.env"]),
        )
        .unwrap();
        assert!(stats.files >= 4);
        assert!(stats.bytes > 0);

        let extracted = TempDir::new().unwrap();
        extract(&out_path, extracted.path(), Format::Zip).unwrap();
        assert_packaged_template(extracted.path());
    }

    #[test]
    fn pack_targz_roundtrip() {
        let src = TempDir::new().unwrap();
        make_template_tree(src.path());
        let arc = TempDir::new().unwrap();
        let out_path = arc.path().join("pkg.tar.gz");

        pack(
            src.path(),
            &out_path,
            Format::TarGz,
            None,
            &ignore_set(&["secrets.env"]),
        )
        .unwrap();

        let extracted = TempDir::new().unwrap();
        extract(&out_path, extracted.path(), Format::TarGz).unwrap();
        assert_packaged_template(extracted.path());
    }

    #[test]
    fn pack_tarzst_roundtrip() {
        let src = TempDir::new().unwrap();
        make_template_tree(src.path());
        let arc = TempDir::new().unwrap();
        let out_path = arc.path().join("pkg.tar.zst");

        pack(
            src.path(),
            &out_path,
            Format::TarZst,
            None,
            &ignore_set(&["secrets.env"]),
        )
        .unwrap();

        let extracted = TempDir::new().unwrap();
        extract(&out_path, extracted.path(), Format::TarZst).unwrap();
        assert_packaged_template(extracted.path());
    }

    #[test]
    fn pack_ignored_directory_is_pruned() {
        // An ignore glob naming a directory drops it whole (no descent).
        let src = TempDir::new().unwrap();
        let at = |rel: &str| {
            let p = src.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            p
        };
        fs::write(at("keep.txt"), "k").unwrap();
        fs::write(at("build/a.txt"), "a").unwrap();
        fs::write(at("build/nested/b.txt"), "b").unwrap();

        let arc = TempDir::new().unwrap();
        let out_path = arc.path().join("p.zip");
        pack(src.path(), &out_path, Format::Zip, None, &ignore_set(&["build"])).unwrap();

        let extracted = TempDir::new().unwrap();
        extract(&out_path, extracted.path(), Format::Zip).unwrap();
        assert!(extracted.path().join("keep.txt").exists());
        assert!(!extracted.path().join("build").exists());
    }

    #[test]
    fn pack_rejects_non_directory() {
        let tmp = TempDir::new().unwrap();
        let not_a_dir = tmp.path().join("file.txt");
        fs::write(&not_a_dir, "x").unwrap();
        let out = tmp.path().join("o.zip");
        let err = pack(
            &not_a_dir,
            &out,
            Format::Zip,
            None,
            &ignore_set(&[]),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("not a directory"));
    }
}
