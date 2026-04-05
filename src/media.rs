use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use image::{DynamicImage, RgbaImage};
use ratatui_image::picker::{Picker, ProtocolType};

#[derive(Debug)]
pub struct TerminalImageSupport {
    picker: Picker,
    protocol: ProtocolType,
}

impl TerminalImageSupport {
    pub fn detect() -> Result<Option<Self>> {
        let picker = Picker::from_query_stdio()?;
        let protocol = picker.protocol_type();
        if matches!(protocol, ProtocolType::Halfblocks) {
            return Ok(None);
        }
        Ok(Some(Self { picker, protocol }))
    }

    pub fn picker(&self) -> &Picker {
        &self.picker
    }

    pub fn label(&self) -> &'static str {
        match self.protocol {
            ProtocolType::Halfblocks => "halfblocks",
            ProtocolType::Sixel => "sixel",
            ProtocolType::Kitty => "kitty",
            ProtocolType::Iterm2 => "iterm2",
        }
    }
}

pub fn resolve_markdown_image_path(path: &str, notebook_path: Option<&Path>) -> PathBuf {
    let path_buf = PathBuf::from(path);
    if path_buf.is_absolute() {
        return path_buf;
    }
    notebook_path
        .and_then(Path::parent)
        .map(|parent| parent.join(&path_buf))
        .unwrap_or(path_buf)
}

pub fn load_markdown_image(path: &Path) -> Result<DynamicImage> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());

    match extension.as_deref() {
        Some("svg") => load_svg(path),
        _ => {
            image::open(path).with_context(|| format!("failed to decode image {}", path.display()))
        }
    }
}

fn load_svg(path: &Path) -> Result<DynamicImage> {
    let svg = fs::read(path).with_context(|| format!("failed to read svg {}", path.display()))?;
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(&svg, &options)
        .with_context(|| format!("failed to parse svg {}", path.display()))?;
    let size = tree.size().to_int_size();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width(), size.height())
        .context("failed to allocate svg pixmap")?;
    let mut pixmap_mut = pixmap.as_mut();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::default(),
        &mut pixmap_mut,
    );
    let rgba = RgbaImage::from_raw(size.width(), size.height(), pixmap.take())
        .context("failed to convert svg pixmap")?;
    Ok(DynamicImage::ImageRgba8(rgba))
}

pub fn markdown_image_alt(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .map(ToString::to_string)
        .unwrap_or_else(|| path.to_string())
}

pub fn validate_markdown_image_path(path: &Path) -> Result<()> {
    if path.exists() {
        Ok(())
    } else {
        bail!("image {} does not exist", path.display())
    }
}
